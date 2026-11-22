// Support Tickets module.
//
// Tickets with comments, assignment, validated status transitions, and
// knowledgebase suggestions. Tickets can also be created through the
// `ticket.create_requested` hook (Chat / AI Support). Interconnection
// happens only through the hook bus.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext, HookHandler};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketComment {
    pub id: String,
    pub author_id: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub id: String,
    pub title: String,
    pub body: String,
    pub user_id: String,
    pub company_id: Option<String>,
    /// `low`, `normal`, `high`, `urgent`.
    pub priority: String,
    /// `open`, `in_progress`, `waiting`, `resolved`, `closed`.
    pub status: String,
    pub category: String,
    pub assigned_to: Option<String>,
    pub comments: Vec<TicketComment>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTicket {
    pub title: String,
    pub body: String,
    pub user_id: String,
    pub company_id: Option<String>,
    pub priority: Option<String>,
    pub category: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTicket {
    pub title: Option<String>,
    pub body: Option<String>,
    pub priority: Option<String>,
    pub category: Option<String>,
    /// Status change; validated against the allowed transitions.
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddComment {
    pub author_id: String,
    pub body: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssignTicket {
    pub agent_id: String,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct TicketsState {
    hooks: RwLock<Arc<HookBus>>,
    tickets: DashMap<String, Ticket>,
}

impl TicketsState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            tickets: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    fn create_ticket(&self, input: CreateTicket) -> UnifiedResult<Ticket> {
        if input.title.trim().is_empty() {
            return Err(UnifiedError::bad_request("Ticket title must not be empty"));
        }
        if input.body.trim().is_empty() {
            return Err(UnifiedError::bad_request("Ticket body must not be empty"));
        }
        if input.user_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("user_id must not be empty"));
        }
        let priority = input.priority.unwrap_or_else(|| "normal".to_string());
        if !valid_priority(&priority) {
            return Err(UnifiedError::bad_request(
                "Priority must be one of: low, normal, high, urgent",
            ));
        }
        let ticket = Ticket {
            id: new_id(),
            title: input.title.trim().to_string(),
            body: input.body.trim().to_string(),
            user_id: input.user_id.trim().to_string(),
            company_id: input.company_id,
            priority,
            status: "open".to_string(),
            category: input
                .category
                .unwrap_or_else(|| "general".to_string()),
            assigned_to: None,
            comments: Vec::new(),
            created_at: now(),
            updated_at: now(),
        };
        self.tickets.insert(ticket.id.clone(), ticket.clone());
        Ok(ticket)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

fn valid_priority(priority: &str) -> bool {
    matches!(priority, "low" | "normal" | "high" | "urgent")
}

/// Allowed status transitions. `closed -> open` is a reopen.
fn allowed_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("open", "in_progress")
            | ("open", "closed")
            | ("in_progress", "waiting")
            | ("in_progress", "resolved")
            | ("in_progress", "closed")
            | ("waiting", "in_progress")
            | ("waiting", "resolved")
            | ("waiting", "closed")
            | ("resolved", "closed")
            | ("resolved", "in_progress")
            | ("closed", "open")
    )
}

fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_tickets(
    state: web::Data<Arc<TicketsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let sort = params.allowed_sort(&["created_at", "priority"], "created_at");
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
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_ticket(
    state: web::Data<Arc<TicketsState>>,
    body: web::Json<CreateTicket>,
) -> HttpResponse {
    let result: UnifiedResult<Ticket> = (|| {
        let ticket = state.create_ticket(body.into_inner())?;
        state.emit(
            "ticket.created",
            serde_json::to_value(&ticket).unwrap_or(Value::Null),
        );
        state.emit(
            "notification.requested",
            json!({
                "user_id": ticket.user_id,
                "title": format!("Ticket created: {}", ticket.title),
                "body": ticket.body,
                "kind": "ticket",
            }),
        );
        Ok(ticket)
    })();
    respond_created(result)
}

async fn get_ticket(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Ticket> = state
        .tickets
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Ticket not found"));
    json::respond(result, false)
}

async fn update_ticket(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateTicket>,
) -> HttpResponse {
    let result: UnifiedResult<Ticket> = (|| {
        let input = body.into_inner();
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
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
            ticket.priority = priority;
        }
        if let Some(category) = input.category {
            ticket.category = category;
        }
        if let Some(status) = input.status {
            if !allowed_transition(&ticket.status, &status) {
                return Err(UnifiedError::unprocessable(format!(
                    "Cannot move ticket from {} to {}",
                    ticket.status, status
                )));
            }
            ticket.status = status;
        }
        ticket.updated_at = now();
        Ok(ticket.clone())
    })();
    json::respond(result, false)
}

async fn delete_ticket(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .tickets
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))
    })();
    json::respond(result, false)
}

async fn add_comment(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
    body: web::Json<AddComment>,
) -> HttpResponse {
    let result: UnifiedResult<Ticket> = (|| {
        let input = body.into_inner();
        if input.author_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("author_id must not be empty"));
        }
        if input.body.trim().is_empty() {
            return Err(UnifiedError::bad_request("Comment body must not be empty"));
        }
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        ticket.comments.push(TicketComment {
            id: new_id(),
            author_id: input.author_id.trim().to_string(),
            body: input.body.trim().to_string(),
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

async fn assign_ticket(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
    body: web::Json<AssignTicket>,
) -> HttpResponse {
    let result: UnifiedResult<Ticket> = (|| {
        let agent_id = body.into_inner().agent_id;
        if agent_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("agent_id must not be empty"));
        }
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        ticket.assigned_to = Some(agent_id.trim().to_string());
        if ticket.status == "open" {
            ticket.status = "in_progress".to_string();
        }
        ticket.updated_at = now();
        let ticket_value = serde_json::to_value(&*ticket).unwrap_or(Value::Null);
        drop(ticket);
        state.emit("ticket.assigned", ticket_value);
        Ok(state
            .tickets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .expect("ticket still present"))
    })();
    json::respond(result, false)
}

async fn ticket_suggestions(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let ticket = state
            .tickets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        // Ask the knowledgebase (via hook) for articles matching the title.
        let context = state.bus().emit(
            "knowledgebase.search",
            json!({ "query": ticket.title, "ticket_id": ticket.id }),
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
        Ok(json!({ "ticket_id": ticket.id, "query": ticket.title, "articles": articles }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

fn on_ticket_create_requested(state: Arc<TicketsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let input = CreateTicket {
        title: data
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        body: data
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        user_id: data
            .get("user_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        company_id: data
            .get("company_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        priority: data
            .get("priority")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        category: data
            .get("category")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    };
    match state.create_ticket(input) {
        Ok(ticket) => {
            let value = serde_json::to_value(&ticket).unwrap_or(Value::Null);
            ctx.add_result(value.clone());
            state.emit("ticket.created", value);
        }
        Err(error) => {
            ctx.add_result(json!({ "error": error.message }));
        }
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct TicketsModule {
    state: Arc<TicketsState>,
}

impl TicketsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(TicketsState::new()),
        }
    }
}

impl Default for TicketsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for TicketsModule {
    fn id(&self) -> &'static str {
        "tickets"
    }

    fn name(&self) -> &'static str {
        "Support Tickets"
    }

    fn description(&self) -> &'static str {
        "Support tickets with comments, assignment, validated status transitions and knowledgebase suggestions."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["knowledgebase", "notifications"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_ticket_create_requested(state.clone(), ctx));
        bus.register("ticket.create_requested", "tickets", handler);
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/tickets")
                .route("", web::get().to(list_tickets))
                .route("", web::post().to(create_ticket))
                .route("/{id}", web::get().to(get_ticket))
                .route("/{id}", web::put().to(update_ticket))
                .route("/{id}", web::delete().to(delete_ticket))
                .route("/{id}/comments", web::post().to(add_comment))
                .route("/{id}/assign", web::post().to(assign_ticket))
                .route("/{id}/suggestions", web::get().to(ticket_suggestions)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(TicketsModule::new()));
}
