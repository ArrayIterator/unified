// Tickets module — ticket, comment, note, attachment, and link handlers.
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
// Handlers
// ---------------------------------------------------------------------------

pub(crate) fn ticket_visible_to(ticket: &Ticket, scope: &(String, bool, Vec<String>)) -> bool {
    let (viewer_id, view_all, departments) = scope;
    *view_all
        || ticket.user_id == *viewer_id
        || ticket
            .department_id
            .as_ref()
            .is_some_and(|department_id| departments.iter().any(|id| id == department_id))
}

pub(crate) fn ticket_staff_view(ticket: &Ticket, scope: &(String, bool, Vec<String>)) -> bool {
    let (_viewer_id, view_all, departments) = scope;
    *view_all
        || ticket
            .department_id
            .as_ref()
            .is_some_and(|department_id| departments.iter().any(|id| id == department_id))
}

pub(crate) async fn list_tickets(
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

    let caller = match authenticate(&req, &state, PERM_TICKETS_READ) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let params = query.into_inner();
    // The scope always comes from the authenticated caller; any
    // viewer_id/viewer_role filter values are ignored.
    let scope = caller_scope(&state, &caller);
    let sort = params.allowed_sort(&["created_at", "priority", "updated_at"], "created_at");
    let mut items: Vec<Ticket> = state
        .tickets
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|ticket| {
            matches_search(&format!("{} {} {}", ticket.title, ticket.body, ticket.category), &params)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| ticket.status == *status)
                && params
                    .filters
                    .get("priority")
                    .is_none_or(|priority| ticket.priority == *priority)
                && params
                    .filters
                    .get("category")
                    .is_none_or(|category| ticket.category == *category)
                && params
                    .filters
                    .get("user_id")
                    .is_none_or(|user_id| ticket.user_id == *user_id)
                && params.filters.get("company_id").is_none_or(|company_id| {
                    ticket.company_id.as_deref() == Some(company_id.as_str())
                })
                && params.filters.get("assigned_to").is_none_or(|agent| {
                    ticket.assigned_to.as_deref() == Some(agent.as_str())
                })
                && params.filters.get("department_id").is_none_or(|department| {
                    ticket.department_id.as_deref() == Some(department.as_str())
                })
                && params.filters.get("product").is_none_or(|product| {
                    ticket.product.as_deref() == Some(product.as_str())
                })
                && params
                    .filters
                    .get("sla_breached")
                    .is_none_or(|flag| {
                        let wanted = flag == "true";
                        (ticket.sla_first_response_breached || ticket.sla_resolution_breached)
                            == wanted
                    })
                && in_company_scope(&ticket.company_id, &_scope)
                && ticket_visible_to(ticket, &scope)
        })
        .map(|ticket| {
            // A scoped viewer sees the public view of tickets outside
            // their own departments; staff see the full record.
            if ticket_staff_view(&ticket, &scope) {
                ticket
            } else {
                ticket.public_view()
            }
        })
        .collect();
    match sort.as_str() {
        "priority" => {
            let rank = |priority: &str| match priority {
                "urgent" => 3,
                "high" => 2,
                "low" => 0,
                _ => 1,
            };
            items.sort_by_key(|ticket| rank(&ticket.priority));
        }
        "updated_at" => items.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_ticket(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
    body: web::Json<CreateTicket>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let caller = match authenticate(&req, &state, PERM_TICKETS_READ) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    // Staff filing on someone's behalf may name the owner; anyone
    // else files as themselves, whatever the body claims.
    let staff = state.check_capability(&caller.role, PERM_TICKETS_VIEW_ALL);
    let result: UnifiedResult<Ticket> = (|| {
        let mut input = body.into_inner();
        if !staff {
            input.user_id = caller.actor.clone();
        }
        input.company_id = require_company_match(&_tenant, &req, &input.company_id)?;
        let ticket = state.create_ticket(input)?;
        state.emit(
            "ticket.created",
            serde_json::to_value(&ticket).unwrap_or(Value::Null),
        );
        state.emit(
            "notification.requested",
            json!({
                "user_id": &ticket.user_id,
                "title": format!("Ticket created: {}", ticket.title),
                "body": &ticket.body,
                "kind": "ticket",
            }),
        );
        Ok(ticket)
    })();
    respond_created(result)
}

pub(crate) async fn get_ticket(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<TicketViewQuery>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let caller = match authenticate(&req, &state, PERM_TICKETS_READ) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    // The viewer query parameters are a display hint only and are
    // ignored: identity always comes from the authenticated caller.
    let _view = query.into_inner();
    let scope = caller_scope(&state, &caller);
    let result: UnifiedResult<Ticket> = (|| {
        let ticket = state
            .tickets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        require_record_in_company_scope(&ticket.company_id, &_scope)?;
        // A caller who may neither see the ticket as staff nor owns
        // it gets a 404, so ticket existence does not leak.
        if !ticket_visible_to(&ticket, &scope) {
            return Err(UnifiedError::not_found("Ticket not found"));
        }
        if ticket_staff_view(&ticket, &scope) {
            Ok(ticket)
        } else {
            Ok(ticket.public_view())
        }
    })();
    json::respond(result, false)
}

pub(crate) async fn update_ticket(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateTicket>,
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
    let result: UnifiedResult<Ticket> = (|| {
        let input = body.into_inner();
        let mut priority_changed = false;
        let mut department_changed = false;
        // The new department's SLA is resolved before locking the
        // ticket so no hook runs while the ticket is locked.
        let new_department_sla = match &input.department_id {
            Some(department_id) if !department_id.trim().is_empty() => {
                Some(state.sla_for_department(department_id.trim()))
            }
            Some(_) => {
                return Err(UnifiedError::bad_request("department_id must not be empty"));
            }
            None => None,
        };
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        require_record_in_company_scope(&ticket.company_id, &_scope)?;
        if let Some(title) = input.title {
            if title.trim().is_empty() {
                return Err(UnifiedError::bad_request("Ticket title must not be empty"));
            }
            ticket.title = title.trim().to_string();
        }
        if let Some(body_text) = input.body {
            if body_text.trim().is_empty() {
                return Err(UnifiedError::bad_request("Ticket body must not be empty"));
            }
            ticket.body = body_text.trim().to_string();
        }
        if let Some(priority) = input.priority {
            if !valid_priority(&priority) {
                return Err(UnifiedError::bad_request(
                    "Priority must be one of: low, normal, high, urgent",
                ));
            }
            if ticket.priority != priority {
                ticket.priority = priority;
                priority_changed = true;
            }
        }
        if let Some(category) = input.category {
            ticket.category = category;
        }
        if let Some(department_id) = input.department_id {
            let department_id = department_id.trim().to_string();
            ticket.department_id = Some(department_id);
            department_changed = true;
            // Restart the SLA clock under the new department.
            let (first_minutes, resolution_minutes) = new_department_sla
                .flatten()
                .unwrap_or_else(|| default_sla_minutes(&ticket.priority));
            let base = now();
            ticket.sla_first_response_due_at =
                Some(base + chrono::Duration::minutes(first_minutes as i64));
            ticket.sla_resolution_due_at =
                Some(base + chrono::Duration::minutes(resolution_minutes as i64));
            ticket.sla_first_response_breached = false;
            ticket.sla_resolution_breached = false;
        }
        if priority_changed && !department_changed {
            // A priority change feeds the SLA targets, so the clock
            // restarts under the (unchanged) department policy,
            // exactly as a department move does. When the department
            // also moved, its block above already recomputed the
            // timers from the new priority.
            state.reapply_sla(&mut ticket);
        }
        if let Some(status) = input.status {
            let status = normalize_status(&status);
            if !valid_status(&status) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown ticket status: {status}"
                )));
            }
            if !allowed_transition(&ticket.status, &status) {
                return Err(UnifiedError::unprocessable(format!(
                    "Cannot move ticket from {} to {}",
                    ticket.status, status
                )));
            }
            if status == "resolved" && ticket.status != "resolved" {
                ticket.resolved_at = Some(now());
            }
            if status == "open" {
                // A reopen starts a fresh resolution clock.
                ticket.resolved_at = None;
            }
            ticket.status = status;
        }
        ticket.updated_at = now();
        let ticket_value = serde_json::to_value(&*ticket).unwrap_or(Value::Null);
        let updated = ticket.clone();
        drop(ticket);
        state.emit("ticket.updated", ticket_value);
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_ticket(
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
            .tickets
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))
    })();
    if result.is_ok() {
        state.emit(
            "ticket.deleted",
            json!({ "ticket_id": path.as_str() }),
        );
    }
    json::respond(result, false)
}

pub(crate) async fn add_comment(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddComment>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let caller = match authenticate_caller(&req) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let result: UnifiedResult<Ticket> = (|| {
        let input = body.into_inner();
        if input.body.trim().is_empty() {
            return Err(UnifiedError::bad_request("Comment body must not be empty"));
        }
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        require_record_in_company_scope(&ticket.company_id, &_scope)?;
        require_ticket_access(&state, &caller, &ticket)?;
        // Resolved and closed tickets are finished: commenting on
        // them is rejected, so the thread cannot silently grow after
        // resolution. Reopen the ticket first to continue it.
        if ticket.status == "resolved" || ticket.status == "closed" {
            return Err(UnifiedError::unprocessable(format!(
                "Cannot add a comment to a {} ticket; reopen it first",
                ticket.status
            )));
        }
        ticket.comments.push(TicketComment {
            id: new_id(),
            author_id: caller.actor.clone(),
            body: input.body.trim().to_string(),
            kind: default_comment_kind(),
            created_at: now(),
        });
        ticket.updated_at = now();
        let ticket_value = serde_json::to_value(&*ticket).unwrap_or(Value::Null);
        drop(ticket);
        state.emit("ticket.commented", ticket_value);
        Ok(state
            .tickets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .expect("ticket still present"))
    })();
    respond_created(result)
}

/// A reply in the public thread. An agent reply records the first
/// response and moves an open or waiting ticket to in progress; a
/// customer reply moves a waiting ticket back to in progress.
/// Replies to resolved or closed tickets are rejected: reopen the
/// ticket first to continue the thread.
pub(crate) async fn add_reply(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddReply>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let caller = match authenticate_caller(&req) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let result: UnifiedResult<Ticket> = (|| {
        let input = body.into_inner();
        if input.body.trim().is_empty() {
            return Err(UnifiedError::bad_request("Reply body must not be empty"));
        }
        let author_id = caller.actor.clone();
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        require_record_in_company_scope(&ticket.company_id, &_scope)?;
        require_ticket_access(&state, &caller, &ticket)?;
        if ticket.status == "resolved" || ticket.status == "closed" {
            return Err(UnifiedError::unprocessable(format!(
                "Cannot add a reply to a {} ticket; reopen it first",
                ticket.status
            )));
        }
        let author_kind = match input.author_kind.as_deref() {
            Some("customer") => "customer",
            Some("agent") => "agent",
            Some(other) => {
                return Err(UnifiedError::bad_request(format!(
                    "author_kind must be customer or agent, got {other}"
                )));
            }
            None => {
                if author_id == ticket.user_id {
                    "customer"
                } else {
                    "agent"
                }
            }
        };
        ticket.comments.push(TicketComment {
            id: new_id(),
            author_id: author_id.clone(),
            body: input.body.trim().to_string(),
            kind: if author_kind == "customer" {
                "customer_reply".to_string()
            } else {
                "agent_reply".to_string()
            },
            created_at: now(),
        });
        if author_kind == "agent" {
            if ticket.first_response_at.is_none() {
                ticket.first_response_at = Some(now());
            }
            if ticket.status == "open" || ticket.status == "waiting" {
                ticket.status = "in_progress".to_string();
            }
        } else if ticket.status == "waiting" {
            ticket.status = "in_progress".to_string();
        }
        ticket.updated_at = now();
        let ticket_value = serde_json::to_value(&*ticket).unwrap_or(Value::Null);
        let notify_user = ticket.user_id.clone();
        let notify_agent = ticket.assigned_to.clone();
        let title = ticket.title.clone();
        let ticket_id = ticket.id.clone();
        drop(ticket);
        state.emit(
            "ticket.replied",
            json!({ "ticket_id": &ticket_id, "author_kind": author_kind, "ticket": ticket_value }),
        );
        if author_kind == "agent" {
            state.notify(
                &notify_user,
                format!("Reply on your ticket: {title}"),
                format!("An agent replied to ticket {ticket_id}."),
            );
        } else if let Some(agent_id) = notify_agent {
            state.notify(
                &agent_id,
                format!("Customer replied: {title}"),
                format!("The customer replied to ticket {ticket_id}."),
            );
        }
        Ok(state
            .tickets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .expect("ticket still present"))
    })();
    respond_created(result)
}

pub(crate) async fn list_notes(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
    path: web::Path<String>,
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

    let caller = match authenticate_caller(&req) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    if let Err(error) = require_staff(&state, &caller) {
        return json::error_response(&error, false);
    }
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<TicketNote>> = (|| {
        let ticket = state
            .tickets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        require_record_in_company_scope(&ticket.company_id, &_scope)?;
        let mut notes = ticket.internal_notes.clone();
        notes.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            notes.reverse();
        }
        Ok(paginate(notes, &params))
    })();
    json::respond(result, false)
}

/// Staff-only internal note. Notes are never part of the customer
/// view and are only reachable through this staff route.
pub(crate) async fn add_note(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddNote>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let caller = match authenticate_caller(&req) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    if let Err(error) = require_staff(&state, &caller) {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<Ticket> = (|| {
        let input = body.into_inner();
        if input.body.trim().is_empty() {
            return Err(UnifiedError::bad_request("Note body must not be empty"));
        }
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        require_record_in_company_scope(&ticket.company_id, &_scope)?;
        ticket.internal_notes.push(TicketNote {
            id: new_id(),
            author_id: caller.actor.clone(),
            body: input.body.trim().to_string(),
            created_at: now(),
        });
        ticket.updated_at = now();
        let ticket_value = serde_json::to_value(&*ticket).unwrap_or(Value::Null);
        let updated = ticket.clone();
        drop(ticket);
        state.emit("ticket.note_added", ticket_value);
        Ok(updated)
    })();
    respond_created(result)
}

pub(crate) async fn list_attachments(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
    path: web::Path<String>,
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

    let caller = match authenticate(&req, &state, PERM_TICKETS_READ) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let scope = caller_scope(&state, &caller);
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<TicketAttachment>> = (|| {
        let ticket = state
            .tickets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        require_record_in_company_scope(&ticket.company_id, &_scope)?;
        if !ticket_visible_to(&ticket, &scope) {
            return Err(UnifiedError::not_found("Ticket not found"));
        }
        let mut attachments = ticket.attachments.clone();
        attachments.sort_by(|a, b| a.uploaded_at.cmp(&b.uploaded_at));
        if params.normalized().order == "desc" {
            attachments.reverse();
        }
        Ok(paginate(attachments, &params))
    })();
    json::respond(result, false)
}

/// Attach a file that was already uploaded through the Storage
/// module. Only the storage reference is stored here.
pub(crate) async fn add_attachment(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddAttachment>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let caller = match authenticate_caller(&req) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let result: UnifiedResult<Ticket> = (|| {
        let input = body.into_inner();
        if input.target.trim().is_empty() {
            return Err(UnifiedError::bad_request("target must not be empty"));
        }
        if input.key.trim().is_empty() {
            return Err(UnifiedError::bad_request("key must not be empty"));
        }
        let key = input.key.trim().to_string();
        let filename = input.filename.filter(|name| !name.trim().is_empty()).unwrap_or_else(|| {
            key.rsplit('/').next().unwrap_or(&key).to_string()
        });
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        require_record_in_company_scope(&ticket.company_id, &_scope)?;
        require_ticket_access(&state, &caller, &ticket)?;
        ticket.attachments.push(TicketAttachment {
            id: new_id(),
            target: input.target.trim().to_string(),
            stored_key: input
                .stored_key
                .filter(|stored| !stored.trim().is_empty())
                .unwrap_or_else(|| key.clone()),
            key,
            filename,
            content_type: input.content_type,
            size_bytes: input.size_bytes,
            uploaded_by: caller.actor.clone(),
            uploaded_at: now(),
        });
        ticket.updated_at = now();
        let ticket_value = serde_json::to_value(&*ticket).unwrap_or(Value::Null);
        let updated = ticket.clone();
        drop(ticket);
        state.emit("ticket.attachment_added", ticket_value);
        Ok(updated)
    })();
    respond_created(result)
}

pub(crate) async fn add_link(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddLink>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let caller = match authenticate_caller(&req) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let result: UnifiedResult<Ticket> = (|| {
        let input = body.into_inner();
        if !matches!(input.target_type.as_str(), "order" | "license" | "server") {
            return Err(UnifiedError::bad_request(
                "target_type must be one of: order, license, server",
            ));
        }
        if input.target_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("target_id must not be empty"));
        }
        let target_id = input.target_id.trim().to_string();
        // License links are the only ones with a lookup hook today.
        // Orders and servers have no lookup hook, so those links are
        // stored unverified until one exists.
        let verified = if input.target_type == "license" {
            state.validate_license_link(&target_id)
        } else {
            false
        };
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        require_record_in_company_scope(&ticket.company_id, &_scope)?;
        require_ticket_access(&state, &caller, &ticket)?;
        if ticket
            .links
            .iter()
            .any(|link| link.target_type == input.target_type && link.target_id == target_id)
        {
            return Err(UnifiedError::conflict(format!(
                "Ticket is already linked to {} {target_id}",
                input.target_type
            )));
        }
        ticket.links.push(TicketLink {
            target_type: input.target_type.clone(),
            target_id,
            verified,
            linked_at: now(),
        });
        ticket.updated_at = now();
        let ticket_value = serde_json::to_value(&*ticket).unwrap_or(Value::Null);
        let updated = ticket.clone();
        drop(ticket);
        state.emit("ticket.linked", ticket_value);
        Ok(updated)
    })();
    respond_created(result)
}

pub(crate) async fn remove_link(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<RemoveLinkQuery>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let caller = match authenticate_caller(&req) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let query = query.into_inner();
    let result: UnifiedResult<Ticket> = (|| {
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        require_record_in_company_scope(&ticket.company_id, &_scope)?;
        require_ticket_access(&state, &caller, &ticket)?;
        let before = ticket.links.len();
        ticket.links.retain(|link| {
            !(link.target_type == query.target_type && link.target_id == query.target_id)
        });
        if ticket.links.len() == before {
            return Err(UnifiedError::not_found(format!(
                "Ticket is not linked to {} {}",
                query.target_type, query.target_id
            )));
        }
        ticket.updated_at = now();
        let updated = ticket.clone();
        drop(ticket);
        Ok(updated)
    })();
    json::respond(result, false)
}

/// Merge this ticket into another one: the source closes and points
/// at the target, and its thread, internal notes, attachments, and
/// links move over. Merges involving a closed ticket are rejected,
/// and the source is closed through the validated status transition.
pub(crate) async fn merge_ticket(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<MergeTicket>,
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
    let result: UnifiedResult<Ticket> = (|| {
        let into_ticket_id = body.into_inner().into_ticket_id;
        let source_id = path.as_str().to_string();
        if into_ticket_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("into_ticket_id must not be empty"));
        }
        if into_ticket_id == source_id {
            return Err(UnifiedError::bad_request(
                "A ticket cannot be merged into itself",
            ));
        }
        // The target must exist before any ticket is locked: checking
        // it while holding the source guard could deadlock on the
        // map's per-shard lock, and a failed merge must never leave
        // the source pointing nowhere.
        if !state.tickets.contains_key(into_ticket_id.as_str()) {
            return Err(UnifiedError::not_found("Target ticket not found"));
        }
        if state
            .tickets
            .get(into_ticket_id.as_str())
            .is_some_and(|entry| entry.status == "closed")
        {
            return Err(UnifiedError::unprocessable(
                "Cannot merge into a closed ticket; reopen it first",
            ));
        }
        let mut source = state
            .tickets
            .get_mut(&source_id)
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        require_record_in_company_scope(&source.company_id, &_scope)?;
        if source.merged_into.is_some() {
            return Err(UnifiedError::conflict(
                "Ticket was already merged into another ticket",
            ));
        }
        if source.status == "closed" {
            return Err(UnifiedError::unprocessable(
                "Cannot merge a closed ticket; reopen it first",
            ));
        }
        if !allowed_transition(&source.status, "closed") {
            return Err(UnifiedError::unprocessable(format!(
                "Cannot move ticket from {} to closed",
                source.status
            )));
        }
        let source_title = source.title.clone();
        let source_comments = source.comments.clone();
        let source_notes = source.internal_notes.clone();
        let source_attachments = source.attachments.clone();
        let source_links = source.links.clone();
        source.status = "closed".to_string();
        source.merged_into = Some(into_ticket_id.clone());
        source.updated_at = now();
        drop(source);
        let mut target = state
            .tickets
            .get_mut(&into_ticket_id)
            .ok_or_else(|| UnifiedError::not_found("Target ticket not found"))?;
        require_record_in_company_scope(&target.company_id, &_scope)?;
        target.comments.extend(source_comments);
        target.internal_notes.extend(source_notes);
        target.attachments.extend(source_attachments);
        for link in source_links {
            if !target.links.iter().any(|existing| {
                existing.target_type == link.target_type && existing.target_id == link.target_id
            }) {
                target.links.push(link);
            }
        }
        target.comments.push(TicketComment {
            id: new_id(),
            author_id: "system".to_string(),
            body: format!("Ticket {source_id} (\"{source_title}\") was merged into this ticket."),
            kind: "system".to_string(),
            created_at: now(),
        });
        target.updated_at = now();
        let target_value = serde_json::to_value(&*target).unwrap_or(Value::Null);
        let target_id = target.id.clone();
        let target_user = target.user_id.clone();
        let target_title = target.title.clone();
        drop(target);
        state.emit(
            "ticket.merged",
            json!({
                "ticket_id": &source_id,
                "into_ticket_id": &into_ticket_id,
                "ticket": target_value,
            }),
        );
        state.notify(
            &target_user,
            format!("Ticket merged: {target_title}"),
            format!("Ticket {source_id} was merged into ticket {target_id}."),
        );
        Ok(state
            .tickets
            .get(&target_id)
            .map(|entry| entry.value().clone())
            .expect("target ticket still present"))
    })();
    json::respond(result, false)
}

pub(crate) async fn escalate_ticket(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<EscalateTicket>,
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
    let result: UnifiedResult<Ticket> = (|| {
        let input = body.into_inner();
        // Resolve the new department's SLA before locking the ticket.
        let new_department_sla = match &input.to_department_id {
            Some(department_id) if !department_id.trim().is_empty() => {
                Some(state.sla_for_department(department_id.trim()))
            }
            Some(_) => {
                return Err(UnifiedError::bad_request(
                    "to_department_id must not be empty",
                ));
            }
            None => None,
        };
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        require_record_in_company_scope(&ticket.company_id, &_scope)?;
        if ticket.status == "closed" {
            return Err(UnifiedError::unprocessable(
                "A closed ticket cannot be escalated; reopen it first",
            ));
        }
        if let Some(department_id) = &input.to_department_id {
            let department_id = department_id.trim().to_string();
            ticket.department_id = Some(department_id);
            let (first_minutes, resolution_minutes) = new_department_sla
                .flatten()
                .unwrap_or_else(|| default_sla_minutes(&ticket.priority));
            let base = now();
            ticket.sla_first_response_due_at =
                Some(base + chrono::Duration::minutes(first_minutes as i64));
            ticket.sla_resolution_due_at =
                Some(base + chrono::Duration::minutes(resolution_minutes as i64));
            ticket.sla_first_response_breached = false;
            ticket.sla_resolution_breached = false;
        }
        if let Some(assign_to) = &input.assign_to {
            if !assign_to.trim().is_empty() {
                ticket.assigned_to = Some(assign_to.trim().to_string());
            }
        }
        ticket.escalation_level += 1;
        ticket.escalated_at = Some(now());
        let escalation_level = ticket.escalation_level;
        ticket.comments.push(TicketComment {
            id: new_id(),
            author_id: "system".to_string(),
            body: match &input.reason {
                Some(reason) if !reason.trim().is_empty() => {
                    format!("Ticket escalated (level {}): {}", escalation_level, reason.trim())
                }
                _ => format!("Ticket escalated (level {}).", escalation_level),
            },
            kind: "system".to_string(),
            created_at: now(),
        });
        ticket.updated_at = now();
        let ticket_value = serde_json::to_value(&*ticket).unwrap_or(Value::Null);
        let notify_agent = ticket.assigned_to.clone();
        let notify_department = ticket.department_id.clone();
        let title = ticket.title.clone();
        let ticket_id = ticket.id.clone();
        let updated = ticket.clone();
        drop(ticket);
        state.emit(
            "ticket.escalated",
            json!({ "ticket_id": &ticket_id, "trigger": "manual", "ticket": ticket_value }),
        );
        let alert_title = format!("Ticket escalated: {title}");
        let alert_body = format!("Ticket {ticket_id} was escalated manually.");
        match &notify_agent {
            Some(agent_id) => state.notify(agent_id, alert_title, alert_body),
            None => {
                if let Some(department_id) = &notify_department {
                    state.notify_department_agents(department_id, alert_title, alert_body);
                }
            }
        }
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn assign_ticket(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AssignTicket>,
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
    let result: UnifiedResult<Ticket> = (|| {
        let agent_id = body.into_inner().agent_id;
        if agent_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("agent_id must not be empty"));
        }
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        require_record_in_company_scope(&ticket.company_id, &_scope)?;
        ticket.assigned_to = Some(agent_id.trim().to_string());
        if ticket.status == "open" {
            ticket.status = "in_progress".to_string();
        }
        ticket.updated_at = now();
        let ticket_value = serde_json::to_value(&*ticket).unwrap_or(Value::Null);
        let notify_agent = ticket.assigned_to.clone();
        let title = ticket.title.clone();
        let ticket_id = ticket.id.clone();
        drop(ticket);
        state.emit("ticket.assigned", ticket_value);
        if let Some(agent_id) = notify_agent {
            state.notify(
                &agent_id,
                format!("Ticket assigned: {title}"),
                format!("Ticket {ticket_id} was assigned to you."),
            );
        }
        Ok(state
            .tickets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .expect("ticket still present"))
    })();
    json::respond(result, false)
}

/// Run the SLA breach scan now. The scheduler (or any module) can
/// trigger the same scan through the `ticket.sla.check_requested`
/// hook instead of calling this endpoint.
pub(crate) async fn sla_check(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
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
    let report = state.run_sla_scan();
    json::success_response(report, 200)
}

pub(crate) async fn ticket_suggestions(
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

    let caller = match authenticate(&req, &state, PERM_TICKETS_READ) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let scope = caller_scope(&state, &caller);
    let result: UnifiedResult<Value> = (|| {
        let ticket = state
            .tickets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        require_record_in_company_scope(&ticket.company_id, &_scope)?;
        if !ticket_visible_to(&ticket, &scope) {
            return Err(UnifiedError::not_found("Ticket not found"));
        }
        // Ask the knowledgebase (via hook) for articles matching the title.
        let context = state.bus().emit(
            "knowledgebase.search",
            json!({ "query": &ticket.title, "ticket_id": &ticket.id }),
        );
        let mut articles: Vec<Value> = Vec::new();
        for result in context.results {
            match result {
                Value::Array(items) => articles.extend(items),
                Value::Object(map) => {
                    if let Some(Value::Array(items)) = map.get("articles") {
                        articles.extend(items.iter().cloned());
                    } else {
                        articles.push(Value::Object(map));
                    }
                }
                other => articles.push(other),
            }
        }
        Ok(json!({ "ticket_id": &ticket.id, "query": &ticket.title, "articles": articles }))
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

    /// State whose `role.check` hook answers "allowed" for every
    /// permission, so handler tests exercise logic, not the roles
    /// module.
    fn allowing_state() -> Arc<TicketsState> {
        let state = Arc::new(TicketsState::new());
        let handler: HookHandler = Arc::new(|ctx: &mut HookContext| {
            ctx.set("allowed", json!(true));
        });
        state.bus().register("role.check", "tickets-test", handler);
        state
    }

    /// State whose `role.check` hook allows reads only: the caller
    /// is an ordinary customer, never staff.
    fn reading_state() -> Arc<TicketsState> {
        let state = Arc::new(TicketsState::new());
        let handler: HookHandler = Arc::new(|ctx: &mut HookContext| {
            let permission = ctx.get("permission").and_then(|v| v.as_str()).unwrap_or("");
            ctx.set("allowed", json!(permission == PERM_TICKETS_READ));
        });
        state.bus().register("role.check", "tickets-test", handler);
        state
    }

    fn state_data(state: &Arc<TicketsState>) -> web::Data<Arc<TicketsState>> {
        web::Data::new(state.clone())
    }

    fn authed_request() -> HttpRequest {
        verified_request("agent-1", "admin")
    }

    fn seed_ticket(state: &TicketsState, user_id: &str) -> Ticket {
        state
            .create_ticket(CreateTicket {
                title: "Help".to_string(),
                body: "Something broke".to_string(),
                user_id: user_id.to_string(),
                company_id: None,
                priority: Some("low".to_string()),
                category: None,
                product: None,
                department_id: None,
            })
            .expect("seed ticket must be created")
    }

    fn set_status(state: &TicketsState, ticket_id: &str, status: &str) {
        state
            .tickets
            .get_mut(ticket_id)
            .expect("seed ticket must exist")
            .status = status.to_string();
    }

    fn reply_body() -> web::Json<AddReply> {
        web::Json(AddReply {
            author_id: "someone-else".to_string(),
            body: "We are looking into it.".to_string(),
            author_kind: Some("agent".to_string()),
        })
    }

    #[tokio::test]
    async fn list_tickets_rejects_anonymous_callers() {
        let state = allowing_state();
        seed_ticket(&state, "user-1");
        let response = list_tickets(
            state_data(&state),
            TestRequest::default().to_http_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn get_ticket_ignores_spoofed_viewer_params() {
        let state = reading_state();
        let owner_id = test_user_uuid("user-1").to_string();
        let ticket = seed_ticket(&state, &owner_id);
        // The caller is user-2; claiming to be user-1 (and an admin)
        // through the query must not reveal user-1's ticket.
        let spoofed = get_ticket(
            state_data(&state),
            verified_request("user-2", "customer"),
            web::Path::from(ticket.id.clone()),
            web::Query(TicketViewQuery {
                viewer_id: Some(owner_id),
                viewer_role: Some("admin".to_string()),
            }),
        )
        .await;
        assert_eq!(spoofed.status(), StatusCode::NOT_FOUND);
        // The real owner, without any viewer parameters, sees it.
        let owned = get_ticket(
            state_data(&state),
            verified_request("user-1", "customer"),
            web::Path::from(ticket.id.clone()),
            web::Query(TicketViewQuery {
                viewer_id: None,
                viewer_role: None,
            }),
        )
        .await;
        assert_eq!(owned.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn update_ticket_recomputes_sla_when_priority_changes() {
        let state = allowing_state();
        let ticket = seed_ticket(&state, "user-1");
        let original_resolution_due = ticket.sla_resolution_due_at;
        let response = update_ticket(
            state_data(&state),
            authed_request(),
            web::Path::from(ticket.id.clone()),
            web::Json(UpdateTicket {
                title: None,
                body: None,
                priority: Some("urgent".to_string()),
                category: None,
                status: None,
                department_id: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let updated = state.tickets.get(&ticket.id).expect("ticket must exist");
        assert_eq!(updated.priority, "urgent");
        // Urgent resolves in 240 minutes; low took 4320, so the due
        // date must have moved.
        let resolution_due = updated
            .sla_resolution_due_at
            .expect("resolution due must be set");
        assert_ne!(Some(resolution_due), original_resolution_due);
        let minutes = resolution_due.signed_duration_since(now()).num_minutes();
        assert!(
            (230..=240).contains(&minutes),
            "urgent resolution due in {minutes} minutes"
        );
        let first_minutes = updated
            .sla_first_response_due_at
            .expect("first-response due must be set")
            .signed_duration_since(now())
            .num_minutes();
        assert!(
            (50..=60).contains(&first_minutes),
            "urgent first response due in {first_minutes} minutes"
        );
    }

    #[tokio::test]
    async fn agent_reply_moves_waiting_ticket_to_in_progress() {
        let state = allowing_state();
        let ticket = seed_ticket(&state, "user-1");
        set_status(&state, &ticket.id, "waiting");
        let response = add_reply(
            state_data(&state),
            authed_request(),
            web::Path::from(ticket.id.clone()),
            reply_body(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let updated = state.tickets.get(&ticket.id).expect("ticket must exist");
        assert_eq!(updated.status, "in_progress");
        // The author is the authenticated caller, not the body value.
        assert_eq!(
            updated.comments.last().expect("reply must be recorded").author_id,
            test_user_uuid("agent-1").to_string()
        );
    }

    #[tokio::test]
    async fn replies_and_comments_are_rejected_on_resolved_and_closed_tickets() {
        let state = allowing_state();
        let ticket = seed_ticket(&state, "user-1");
        set_status(&state, &ticket.id, "resolved");
        let reply = add_reply(
            state_data(&state),
            authed_request(),
            web::Path::from(ticket.id.clone()),
            reply_body(),
        )
        .await;
        assert_eq!(reply.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let comment = add_comment(
            state_data(&state),
            authed_request(),
            web::Path::from(ticket.id.clone()),
            web::Json(AddComment {
                author_id: "agent-1".to_string(),
                body: "A late note".to_string(),
            }),
        )
        .await;
        assert_eq!(comment.status(), StatusCode::UNPROCESSABLE_ENTITY);
        set_status(&state, &ticket.id, "closed");
        let reply = add_reply(
            state_data(&state),
            authed_request(),
            web::Path::from(ticket.id.clone()),
            reply_body(),
        )
        .await;
        assert_eq!(reply.status(), StatusCode::UNPROCESSABLE_ENTITY);
        // Nothing was recorded on the finished ticket.
        let updated = state.tickets.get(&ticket.id).expect("ticket must exist");
        assert!(updated.comments.is_empty());
    }

    #[tokio::test]
    async fn merge_ticket_moves_internal_notes_and_rejects_closed_tickets() {
        let state = allowing_state();
        let source = seed_ticket(&state, "user-1");
        let target = seed_ticket(&state, "user-1");
        state
            .tickets
            .get_mut(&source.id)
            .expect("source must exist")
            .internal_notes
            .push(TicketNote {
                id: "note-1".to_string(),
                author_id: "agent-1".to_string(),
                body: "Internal context".to_string(),
                created_at: now(),
            });
        let response = merge_ticket(
            state_data(&state),
            authed_request(),
            web::Path::from(source.id.clone()),
            web::Json(MergeTicket {
                into_ticket_id: target.id.clone(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let merged_source = state.tickets.get(&source.id).expect("source must exist");
        assert_eq!(merged_source.status, "closed");
        assert_eq!(
            merged_source.merged_into.as_deref(),
            Some(target.id.as_str())
        );
        let merged_target = state.tickets.get(&target.id).expect("target must exist");
        assert_eq!(merged_target.internal_notes.len(), 1);
        assert_eq!(merged_target.internal_notes[0].body, "Internal context");

        // Merging an already-closed source is rejected.
        let closed_source = seed_ticket(&state, "user-1");
        set_status(&state, &closed_source.id, "closed");
        let open_target = seed_ticket(&state, "user-1");
        let response = merge_ticket(
            state_data(&state),
            authed_request(),
            web::Path::from(closed_source.id.clone()),
            web::Json(MergeTicket {
                into_ticket_id: open_target.id.clone(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

        // Merging into a closed target is rejected too.
        let open_source = seed_ticket(&state, "user-1");
        set_status(&state, &open_target.id, "closed");
        let response = merge_ticket(
            state_data(&state),
            authed_request(),
            web::Path::from(open_source.id.clone()),
            web::Json(MergeTicket {
                into_ticket_id: open_target.id.clone(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}
