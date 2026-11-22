// AI Support module.
//
// A support assistant whose answers are grounded in real knowledgebase
// articles: every question is sent to the `knowledgebase.search` hook and
// the answer is built only from the articles that hook returns. When no
// article matches, the assistant says so honestly and offers a handoff
// instead of fabricating an answer.
//
// Routes live under `/api/v1/ai-support`:
//   - `POST /ask {message, conversation_id?}` — grounded answer.
//   - `GET  /conversations` — paginated conversation history.
//   - `GET/PATCH/DELETE /conversations/{id}`.
//   - `POST /handoff {conversation_id}` — request a support ticket.
//   - `POST /tickets {conversation_id, summary}` — same, with a summary.
//   - `GET/POST  /connectors`, `GET/PATCH/DELETE /connectors/{id}` —
//     external chat connector registry (webchat, telegram, whatsapp, ...).
//
// ERP interconnection (all through hooks, never direct module calls):
//   - Grounding: emits `knowledgebase.search`, reads the returned
//     articles from the hook results.
//   - Handoff: emits `ticket.create_requested`; the tickets module
//     creates the ticket and may return its id in the hook results.
//
// Connector note: enabling a connector stores its configuration. Making
// a live connection to the external chat service is a documented next
// milestone, not part of this module (see the TODO on `Connector`).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::HookBus;
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_MESSAGE_LEN: usize = 10_000;
const MAX_SUMMARY_LEN: usize = 5_000;
const MAX_TITLE_LEN: usize = 200;
const MAX_CONNECTOR_NAME_LEN: usize = 100;
/// How many knowledgebase articles ground one answer.
const GROUNDING_LIMIT: u64 = 5;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Conversation lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConversationStatus {
    Open,
    Handoff,
    Closed,
}

impl Default for ConversationStatus {
    fn default() -> Self {
        Self::Open
    }
}

/// Who wrote a conversation message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

/// One knowledgebase article cited as grounding for an answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArticleSource {
    pub article_id: String,
    pub title: String,
    pub url: String,
    pub score: f64,
}

/// A support conversation with the assistant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: Uuid,
    pub user_id: String,
    pub title: Option<String>,
    pub status: ConversationStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One message inside a conversation, with its grounding sources.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub role: MessageRole,
    pub content: String,
    pub sources: Vec<ArticleSource>,
    pub created_at: DateTime<Utc>,
}

/// A conversation with its messages, as returned by the detail endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct ConversationDetail {
    #[serde(flatten)]
    pub conversation: Conversation,
    pub messages: Vec<ConversationMessage>,
}

/// External chat channel kinds the registry accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectorKind {
    Webchat,
    Telegram,
    Whatsapp,
    Slack,
    Messenger,
}

/// An external chat connector registration.
//
// TODO: establishing a live connection to the external chat service (bot
// token validation, webhook registration, inbound message delivery) is a
// follow-up milestone. Enabling a connector currently only stores its
// configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connector {
    pub id: Uuid,
    pub name: String,
    pub kind: ConnectorKind,
    pub enabled: bool,
    pub config: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AskRequest {
    pub message: String,
    pub conversation_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct HandoffRequest {
    pub conversation_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct CreateTicketRequest {
    pub conversation_id: Uuid,
    pub summary: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateConversationRequest {
    pub title: Option<Option<String>>,
    pub status: Option<ConversationStatus>,
}

#[derive(Debug, Deserialize)]
pub struct CreateConnectorRequest {
    pub name: String,
    pub kind: ConnectorKind,
    pub enabled: Option<bool>,
    pub config: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateConnectorRequest {
    pub name: Option<String>,
    pub kind: Option<ConnectorKind>,
    pub enabled: Option<bool>,
    pub config: Option<Value>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for AI Support. The hook bus is attached in `init`
/// (the bus lives in `AppContext`); grounding and handoff degrade honestly
/// when no bus is attached: questions report that no articles were found
/// and handoffs report that no ticket could be requested.
#[derive(Clone, Default)]
pub struct AiSupportState {
    pub conversations: Arc<DashMap<Uuid, Conversation>>,
    pub messages: Arc<DashMap<Uuid, Vec<ConversationMessage>>>,
    pub connectors: Arc<DashMap<Uuid, Connector>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl AiSupportState {
    fn emit(&self, event: &str, data: Value) -> Option<crate::core::hooks::HookContext> {
        self.hooks
            .read()
            .expect("hooks lock poisoned")
            .clone()
            .map(|bus| bus.emit(event, data))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn actor_id(req: &HttpRequest) -> UnifiedResult<String> {
    req.headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn err(error: UnifiedError) -> HttpResponse {
    json::error_response(&error, false)
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

fn load_conversation(
    state: &AiSupportState,
    id: Uuid,
    actor: &str,
) -> UnifiedResult<Conversation> {
    let conversation = state
        .conversations
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Conversation not found: {id}")))?;
    if conversation.user_id != actor {
        return Err(UnifiedError::forbidden(
            "Conversation belongs to a different user",
        ));
    }
    Ok(conversation)
}

fn push_message(
    state: &AiSupportState,
    conversation_id: Uuid,
    role: MessageRole,
    content: String,
    sources: Vec<ArticleSource>,
) -> ConversationMessage {
    let message = ConversationMessage {
        id: Uuid::now_v7(),
        conversation_id,
        role,
        content,
        sources,
        created_at: Utc::now(),
    };
    match state.messages.get_mut(&conversation_id) {
        Some(mut messages) => messages.push(message.clone()),
        None => {
            state
                .messages
                .insert(conversation_id, vec![message.clone()]);
        }
    }
    message
}

/// Parse `knowledgebase.search` hook results into grounding sources. The
/// knowledgebase handler contributes `{ "articles": [...] }`; any other
/// shape is ignored rather than trusted.
fn parse_sources(ctx: &crate::core::hooks::HookContext) -> Vec<ArticleSource> {
    let mut sources: Vec<ArticleSource> = Vec::new();
    for result in &ctx.results {
        let articles = match result.get("articles").and_then(|value| value.as_array()) {
            Some(articles) => articles.clone(),
            None => match result.as_array() {
                Some(articles) => articles.clone(),
                None => continue,
            },
        };
        for article in articles {
            let article_id = article
                .get("id")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let title = article
                .get("title")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            if article_id.is_empty() || title.is_empty() {
                continue;
            }
            sources.push(ArticleSource {
                article_id,
                title,
                url: article
                    .get("url")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string(),
                score: article
                    .get("score")
                    .and_then(|value| value.as_f64())
                    .unwrap_or(0.0),
            });
        }
    }
    sources
}

/// Build the assistant answer from grounding sources only. The answer
/// cites the articles and points at them; it never invents content beyond
/// what the articles returned.
fn grounded_answer(sources: &[ArticleSource]) -> String {
    let mut answer = String::from(
        "Based on the knowledgebase, the following articles are relevant to your question:\n",
    );
    for (index, source) in sources.iter().enumerate() {
        answer.push_str(&format!(
            "{}. {} ({})\n",
            index + 1,
            source.title,
            source.url
        ));
    }
    answer.push_str(
        "\nOpen an article above for the full answer. If none of these cover your \
         question, ask for a handoff and a human agent will pick it up.",
    );
    answer
}

fn no_articles_answer() -> String {
    String::from(
        "I could not find any knowledgebase articles matching your question, so I \
         cannot give a grounded answer. You can rephrase your question, or ask for \
         a handoff and a human support agent will help you.",
    )
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct AiSupportModule {
    state: AiSupportState,
}

impl AiSupportModule {
    pub fn new() -> Self {
        Self {
            state: AiSupportState::default(),
        }
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AiSupportModule::new()));
}

impl Module for AiSupportModule {
    fn id(&self) -> &'static str {
        "ai-support"
    }

    fn name(&self) -> &'static str {
        "AI Support"
    }

    fn description(&self) -> &'static str {
        "AI support assistant grounded in knowledgebase articles, with handoff to human support tickets and an external chat connector registry."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["knowledgebase", "tickets", "chat"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "POST", path: "/api/v1/ai-support/ask", owner: "ai-support", description: "Ask" },
            RouteInfo { method: "GET", path: "/api/v1/ai-support/conversations", owner: "ai-support", description: "List conversations" },
            RouteInfo { method: "GET", path: "/api/v1/ai-support/conversations/{id}", owner: "ai-support", description: "Get conversation" },
            RouteInfo { method: "PATCH", path: "/api/v1/ai-support/conversations/{id}", owner: "ai-support", description: "Update conversation" },
            RouteInfo { method: "DELETE", path: "/api/v1/ai-support/conversations/{id}", owner: "ai-support", description: "Delete conversation" },
            RouteInfo { method: "POST", path: "/api/v1/ai-support/handoff", owner: "ai-support", description: "Handoff" },
            RouteInfo { method: "POST", path: "/api/v1/ai-support/tickets", owner: "ai-support", description: "Create ticket" },
            RouteInfo { method: "GET", path: "/api/v1/ai-support/connectors", owner: "ai-support", description: "List connectors" },
            RouteInfo { method: "POST", path: "/api/v1/ai-support/connectors", owner: "ai-support", description: "Create connector" },
            RouteInfo { method: "GET", path: "/api/v1/ai-support/connectors/{id}", owner: "ai-support", description: "Get connector" },
            RouteInfo { method: "PATCH", path: "/api/v1/ai-support/connectors/{id}", owner: "ai-support", description: "Update connector" },
            RouteInfo { method: "DELETE", path: "/api/v1/ai-support/connectors/{id}", owner: "ai-support", description: "Delete connector" },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/ai-support")
                .app_data(state)
                .route("/ask", web::post().to(ask))
                .route("/conversations", web::get().to(list_conversations))
                .route("/conversations/{id}", web::get().to(get_conversation))
                .route(
                    "/conversations/{id}",
                    web::patch().to(update_conversation),
                )
                .route(
                    "/conversations/{id}",
                    web::delete().to(delete_conversation),
                )
                .route("/handoff", web::post().to(handoff))
                .route("/tickets", web::post().to(create_ticket))
                .route("/connectors", web::get().to(list_connectors))
                .route("/connectors", web::post().to(create_connector))
                .route("/connectors/{id}", web::get().to(get_connector))
                .route("/connectors/{id}", web::patch().to(update_connector))
                .route("/connectors/{id}", web::delete().to(delete_connector)),
        );
    }
}

// ---------------------------------------------------------------------------
// Ask handler (grounded answering)
// ---------------------------------------------------------------------------

async fn ask(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    body: web::Json<AskRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let actor = actor_id(&req)?;
        let question = require_text(&body.message, "message", MAX_MESSAGE_LEN)?;
        // Resolve or create the conversation; it must belong to the caller.
        let mut conversation = match body.conversation_id {
            Some(id) => load_conversation(&state, id, &actor)?,
            None => {
                let now = Utc::now();
                let title: String = question.chars().take(80).collect();
                let conversation = Conversation {
                    id: Uuid::now_v7(),
                    user_id: actor.clone(),
                    title: Some(title),
                    status: ConversationStatus::Open,
                    created_at: now,
                    updated_at: now,
                };
                state
                    .conversations
                    .insert(conversation.id, conversation.clone());
                conversation
            }
        };
        if conversation.status == ConversationStatus::Closed {
            conversation.status = ConversationStatus::Open;
        }
        push_message(
            &state,
            conversation.id,
            MessageRole::User,
            question.clone(),
            Vec::new(),
        );
        // Grounding: ask the knowledgebase through the hook bus. When no
        // handler answers, `sources` stays empty and the reply says so.
        let mut sources: Vec<ArticleSource> = Vec::new();
        if let Some(ctx) = state.emit(
            "knowledgebase.search",
            json!({ "query": question, "limit": GROUNDING_LIMIT }),
        ) {
            sources = parse_sources(&ctx);
        }
        let grounded = !sources.is_empty();
        let answer = if grounded {
            grounded_answer(&sources)
        } else {
            no_articles_answer()
        };
        push_message(
            &state,
            conversation.id,
            MessageRole::Assistant,
            answer.clone(),
            sources.clone(),
        );
        conversation.updated_at = Utc::now();
        state
            .conversations
            .insert(conversation.id, conversation.clone());
        Ok(json!({
            "conversation_id": conversation.id.to_string(),
            "answer": answer,
            "sources": sources,
            "grounded": grounded,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Conversation handlers
// ---------------------------------------------------------------------------

async fn list_conversations(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<Conversation>> = (|| {
        let actor = actor_id(&req)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Conversation> = state
            .conversations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|conversation| conversation.user_id == actor)
            .filter(|conversation| {
                if let Some(status) = &status_filter {
                    let expected = match status.as_str() {
                        "open" => ConversationStatus::Open,
                        "handoff" => ConversationStatus::Handoff,
                        "closed" => ConversationStatus::Closed,
                        _ => return false,
                    };
                    if conversation.status != expected {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let title = conversation.title.clone().unwrap_or_default().to_lowercase();
                    if !title.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["created_at", "updated_at"], "updated_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "created_at" => a.created_at.cmp(&b.created_at),
                _ => a.updated_at.cmp(&b.updated_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

async fn get_conversation(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ConversationDetail> = (|| {
        let actor = actor_id(&req)?;
        let id = parse_uuid(&path.into_inner(), "conversation")?;
        let conversation = load_conversation(&state, id, &actor)?;
        let mut messages: Vec<ConversationMessage> = state
            .messages
            .get(&id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        messages.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(ConversationDetail {
            conversation,
            messages,
        })
    })();
    match result {
        Ok(detail) => ok(detail),
        Err(error) => err(error),
    }
}

async fn update_conversation(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateConversationRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Conversation> = (|| {
        let actor = actor_id(&req)?;
        let id = parse_uuid(&path.into_inner(), "conversation")?;
        let mut conversation = load_conversation(&state, id, &actor)?;
        if let Some(title) = &body.title {
            conversation.title = match title {
                Some(raw) => Some(require_text(raw, "title", MAX_TITLE_LEN)?),
                None => None,
            };
        }
        if let Some(status) = body.status {
            conversation.status = status;
        }
        conversation.updated_at = Utc::now();
        state
            .conversations
            .insert(conversation.id, conversation.clone());
        Ok(conversation)
    })();
    match result {
        Ok(conversation) => ok(conversation),
        Err(error) => err(error),
    }
}

async fn delete_conversation(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let actor = actor_id(&req)?;
        let id = parse_uuid(&path.into_inner(), "conversation")?;
        let _conversation = load_conversation(&state, id, &actor)?;
        state.conversations.remove(&id);
        state.messages.remove(&id);
        Ok(json!({ "deleted": true, "conversation_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Handoff handlers (conversation -> support ticket, via hook)
// ---------------------------------------------------------------------------

/// Shared handoff implementation: emit `ticket.create_requested` and mark
/// the conversation as handed off. The tickets module owns ticket creation;
/// when its hook handler contributes a ticket id in the results, it is
/// returned as `ticket_id`, otherwise `ticket_id` is null and the caller
/// knows only that the request was emitted.
fn do_handoff(
    state: &AiSupportState,
    actor: &str,
    conversation_id: Uuid,
    summary: String,
) -> UnifiedResult<Value> {
    let mut conversation = load_conversation(state, conversation_id, actor)?;
    let history: Vec<ConversationMessage> = state
        .messages
        .get(&conversation_id)
        .map(|entry| entry.value().clone())
        .unwrap_or_default();
    let mut transcript = String::new();
    for message in history.iter().rev().take(3).rev() {
        let role = match message.role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::System => "system",
        };
        transcript.push_str(&format!("{role}: {}\n", message.content));
    }
    let title = conversation
        .title
        .clone()
        .unwrap_or_else(|| "AI Support handoff".to_string());
    let mut ticket_id: Option<String> = None;
    match state.emit(
        "ticket.create_requested",
        json!({
            "source": "ai-support",
            "conversation_id": conversation_id.to_string(),
            "user_id": actor,
            "title": title,
            "summary": summary,
            "transcript": transcript.trim(),
        }),
    ) {
        Some(ctx) => {
            for result in &ctx.results {
                if let Some(id) = result
                    .get("ticket_id")
                    .or_else(|| result.get("id"))
                    .and_then(|value| value.as_str())
                {
                    ticket_id = Some(id.to_string());
                    break;
                }
            }
        }
        None => {
            return Err(UnifiedError::internal(
                "Ticket service is unavailable: no handler answered ticket.create_requested",
            ));
        }
    }
    conversation.status = ConversationStatus::Handoff;
    conversation.updated_at = Utc::now();
    state
        .conversations
        .insert(conversation.id, conversation.clone());
    Ok(json!({
        "ticket_requested": true,
        "ticket_id": ticket_id,
        "conversation_id": conversation_id.to_string(),
        "status": "handoff",
    }))
}

async fn handoff(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    body: web::Json<HandoffRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let actor = actor_id(&req)?;
        let history: Vec<ConversationMessage> = state
            .messages
            .get(&body.conversation_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        let summary = history
            .iter()
            .rev()
            .take(3)
            .map(|message| message.content.clone())
            .collect::<Vec<String>>();
        let mut summary = summary;
        summary.reverse();
        let summary = summary.join(" ");
        let summary = if summary.trim().is_empty() {
            "AI Support handoff requested by the user.".to_string()
        } else {
            summary.chars().take(MAX_SUMMARY_LEN).collect()
        };
        do_handoff(&state, &actor, body.conversation_id, summary)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}

async fn create_ticket(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    body: web::Json<CreateTicketRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let actor = actor_id(&req)?;
        let summary = require_text(&body.summary, "summary", MAX_SUMMARY_LEN)?;
        do_handoff(&state, &actor, body.conversation_id, summary)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Connector handlers
// ---------------------------------------------------------------------------

async fn create_connector(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    body: web::Json<CreateConnectorRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Connector> = (|| {
        let _actor = actor_id(&req)?;
        let name = require_text(&body.name, "name", MAX_CONNECTOR_NAME_LEN)?;
        if state
            .connectors
            .iter()
            .any(|entry| entry.value().name == name)
        {
            return Err(UnifiedError::conflict(format!(
                "A connector named \"{name}\" already exists"
            )));
        }
        let config = match &body.config {
            Some(config) if !config.is_object() => {
                return Err(UnifiedError::bad_request("config must be a JSON object"));
            }
            Some(config) => config.clone(),
            None => json!({}),
        };
        let now = Utc::now();
        let connector = Connector {
            id: Uuid::now_v7(),
            name,
            kind: body.kind,
            enabled: body.enabled.unwrap_or(false),
            config,
            created_at: now,
            updated_at: now,
        };
        state.connectors.insert(connector.id, connector.clone());
        Ok(connector)
    })();
    match result {
        Ok(connector) => created(connector),
        Err(error) => err(error),
    }
}

async fn list_connectors(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<Connector>> = (|| {
        let _actor = actor_id(&req)?;
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let enabled_filter = params.filters.get("enabled").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Connector> = state
            .connectors
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|connector| {
                if let Some(kind) = &kind_filter {
                    let expected = match kind.as_str() {
                        "webchat" => ConnectorKind::Webchat,
                        "telegram" => ConnectorKind::Telegram,
                        "whatsapp" => ConnectorKind::Whatsapp,
                        "slack" => ConnectorKind::Slack,
                        "messenger" => ConnectorKind::Messenger,
                        _ => return false,
                    };
                    if connector.kind != expected {
                        return false;
                    }
                }
                if let Some(enabled) = &enabled_filter {
                    let expected = match enabled.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if connector.enabled != expected {
                        return false;
                    }
                }
                if !search.is_empty() && !connector.name.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["name", "created_at", "updated_at"], "name");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "created_at" => a.created_at.cmp(&b.created_at),
                "updated_at" => a.updated_at.cmp(&b.updated_at),
                _ => a.name.cmp(&b.name),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

async fn get_connector(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Connector> = (|| {
        let _actor = actor_id(&req)?;
        let id = parse_uuid(&path.into_inner(), "connector")?;
        state
            .connectors
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Connector not found: {id}")))
    })();
    match result {
        Ok(connector) => ok(connector),
        Err(error) => err(error),
    }
}

async fn update_connector(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateConnectorRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Connector> = (|| {
        let _actor = actor_id(&req)?;
        let id = parse_uuid(&path.into_inner(), "connector")?;
        let mut connector = state
            .connectors
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Connector not found: {id}")))?;
        if let Some(name) = &body.name {
            let name = require_text(name, "name", MAX_CONNECTOR_NAME_LEN)?;
            if state
                .connectors
                .iter()
                .any(|entry| entry.value().name == name && entry.value().id != id)
            {
                return Err(UnifiedError::conflict(format!(
                    "A connector named \"{name}\" already exists"
                )));
            }
            connector.name = name;
        }
        if let Some(kind) = body.kind {
            connector.kind = kind;
        }
        if let Some(enabled) = body.enabled {
            connector.enabled = enabled;
        }
        if let Some(config) = &body.config {
            if !config.is_object() {
                return Err(UnifiedError::bad_request("config must be a JSON object"));
            }
            connector.config = config.clone();
        }
        connector.updated_at = Utc::now();
        state.connectors.insert(connector.id, connector.clone());
        Ok(connector)
    })();
    match result {
        Ok(connector) => ok(connector),
        Err(error) => err(error),
    }
}

async fn delete_connector(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _actor = actor_id(&req)?;
        let id = parse_uuid(&path.into_inner(), "connector")?;
        if state.connectors.remove(&id).is_none() {
            return Err(UnifiedError::not_found(format!("Connector not found: {id}")));
        }
        Ok(json!({ "deleted": true, "connector_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}
