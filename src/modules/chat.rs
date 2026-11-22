// Chat module.
//
// Real-time messaging: channels (public, private, direct messages),
// messages with reactions, file registrations, presence, and a WebSocket
// hub backed by a tokio broadcast channel.
//
// Routes live under `/api/v1/chat`:
//   - `GET/POST  /channels` — list visible channels / create.
//   - `GET/PATCH/DELETE /channels/{id}`.
//   - `POST /channels/{id}/members`, `DELETE /channels/{id}/members/{user_id}`.
//   - `GET/POST  /channels/{id}/messages` — newest first, paginated.
//   - `PATCH/DELETE /channels/{id}/messages/{message_id}`.
//   - `POST /channels/{id}/messages/{message_id}/reactions` (toggle).
//   - `POST /channels/{id}/messages/{message_id}/escalate` — turn a
//     message into a support ticket via the `ticket.create_requested` hook.
//   - `GET/POST  /channels/{id}/files` — register a shared file.
//   - `GET/POST  /dm` — list / find-or-create a 1:1 channel.
//   - `GET/POST  /presence` — presence with last-seen timestamps.
//   - `GET /ws?channel={id}` — WebSocket feed of broadcast chat events.
//
// Live support chat lives under `/api/v1/chat/support`:
//   - `GET/PUT /support/widget/config` — website widget configuration.
//   - `POST    /support/widget/sessions` — visitor bootstrap (creates a
//     backing private channel and routes the session to an agent).
//   - `GET     /support/widget/sessions/{id}` — visitor polling.
//   - `GET/POST /support/sessions` — list / staff-create a session.
//   - `GET     /support/sessions/{id}`.
//   - `POST    /support/sessions/{id}/accept|close|transfer|escalate`.
//   - `GET     /support/sessions/{id}/transcript?format=json|text`.
//   - `GET     /support/sessions/{id}/position` — queue position.
//   - `POST    /support/sessions/{id}/rating`, `GET .../rating`.
//   - `GET     /support/ratings` — all ratings, paginated.
//   - `GET     /support/queue` — waiting visitors.
//   - `GET/POST /support/agents`, `GET/PATCH/DELETE /support/agents/{user_id}`.
//   - `GET/POST /support/canned`, `GET/PATCH/DELETE /support/canned/{id}`,
//     `POST /support/canned/{id}/use`.
//   - `GET/POST /support/offline-messages` — offline message to ticket.
//
// Visitor-facing widget endpoints (`/support/widget/*` bootstrap and
// polling, offline messages, session rating) deliberately do not require
// the `X-User-Id` header: website visitors have no account. They are
// addressed by the unguessable session id returned at bootstrap.
//
// Department routing: a support session carries a `department_id` (an
// opaque identifier owned by the Departments module). Chat asks the
// Departments module for the department's agents through the real
// `department.agents_requested` event (payload `{"department_id": ...}`,
// answer: a result carrying an `agent_ids` array — see
// `tickets.rs`, `agents_for_department`), then picks the least busy,
// effectively available agent among them using presence plus the
// agent's own availability toggle and concurrent-chat limit. When the
// Departments module is not registered yet, chat falls back to its own
// agent roster (agents registered per department under
// `/support/agents`). Agents the Departments module reports but who
// have no chat roster record are treated as available with the
// default concurrent-chat limit.
//
// Canned responses: no other module in this tree owns canned responses
// (the tickets module has none), so chat is the canonical store. The
// tickets module — or any other module — can reuse them by listening to
// `chat.canned_response.created/updated/deleted` or by reading them
// through `GET /api/v1/chat/support/canned`.
//
// File sharing: a shared file is only a registration here (name, size,
// mime plus an optional storage reference). The bytes live in the
// storage module; `storage_target` + `storage_key` identify the object
// and `url` carries a public or signed URL when the uploader has one.
//
// Actor identity: until the auth module provides an extractor, the caller
// is identified by the `X-User-Id` header (required). Role checks use the
// optional `X-User-Roles` header (comma separated) as an interim source
// until the roles module exposes a lookup hook; role-restricted channels
// only admit members when no roles header is supplied.
//
// Events emitted on the hook bus:
//   - `chat.channel.created`, `chat.channel.updated`, `chat.channel.deleted`
//   - `chat.message` (on every new message)
//   - `file.shared` (on file registration, consumed by the File Manager)
//   - `notification.requested` (once per `@user` mention)
//   - `ticket.create_requested` (on message escalation, chat-to-ticket
//     escalation, and offline messages; answered by the tickets module)
//   - `chat.session.created`, `chat.session.queued`, `chat.session.assigned`,
//     `chat.session.accepted`, `chat.session.transferred`,
//     `chat.session.closed`, `chat.session.escalated`, `chat.session.rated`
//   - `chat.canned_response.created`, `chat.canned_response.updated`,
//     `chat.canned_response.deleted`, `chat.canned_response.used`
//   - `chat.offline_message.created`
//
// Events consumed from other modules:
//   - `department.agents_requested` (asked when routing a session; the
//     Departments module answers with an `agent_ids` result)

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use actix_ws::Message;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use futures_util::StreamExt;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_NAME_LEN: usize = 100;
const MAX_BODY_LEN: usize = 10_000;
const MAX_MEMBERS: usize = 500;
const MAX_FILE_SIZE: u64 = 512 * 1024 * 1024;
const MAX_FILE_NAME_LEN: usize = 255;
const MAX_MIME_LEN: usize = 128;
/// Presence entries older than this are stale and hidden.
const PRESENCE_STALE_SECS: i64 = 60;
/// Broadcast channel capacity for the WebSocket hub.
const HUB_CAPACITY: usize = 256;
/// Live support limits.
const MAX_SUBJECT_LEN: usize = 200;
const MAX_VISITOR_NAME_LEN: usize = 100;
const MAX_EMAIL_LEN: usize = 254;
const MAX_DEPARTMENT_LEN: usize = 128;
const MAX_GREETING_LEN: usize = 500;
const MAX_COLOR_LEN: usize = 16;
const MAX_CANNED_TITLE_LEN: usize = 100;
const MAX_CANNED_BODY_LEN: usize = 5_000;
const MAX_NOTE_LEN: usize = 2_000;
const MAX_RATING_COMMENT_LEN: usize = 1_000;
const MAX_OFFLINE_MESSAGE_LEN: usize = 5_000;
const MAX_STORAGE_REF_LEN: usize = 512;
const MAX_URL_LEN: usize = 2_048;
const DEFAULT_MAX_CONCURRENT_CHATS: u32 = 5;
const MAX_RATING_SCORE: u8 = 5;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Channel visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelKind {
    Public,
    Private,
    Dm,
}

impl Default for ChannelKind {
    fn default() -> Self {
        Self::Public
    }
}

/// Presence status values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PresenceStatus {
    Online,
    Away,
    Busy,
    Offline,
}

/// A chat channel. `company_id` scopes the channel to one company (the
/// company module owns companies; only the id is stored here). `roles`
/// restricts a public channel to callers holding one of the listed roles;
/// an empty list means unrestricted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub id: Uuid,
    pub name: String,
    pub kind: ChannelKind,
    pub company_id: Option<String>,
    pub owner_id: String,
    pub members: Vec<String>,
    pub roles: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One emoji reaction on a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reaction {
    pub user_id: String,
    pub emoji: String,
}

/// A chat message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub author_id: String,
    pub body: String,
    pub edited: bool,
    pub reactions: Vec<Reaction>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A file shared in a channel. Only the registration is stored here; the
/// File Manager module owns the bytes and answers the `file.shared` hook.
/// The storage reference (`storage_target` + `storage_key`, plus an
/// optional public/signed `url`) points at the object in the storage
/// module — chat never stores file bytes itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatFile {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub uploader_id: String,
    pub name: String,
    pub size: u64,
    pub mime: String,
    #[serde(default)]
    pub storage_target: Option<String>,
    #[serde(default)]
    pub storage_key: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One user's presence entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceEntry {
    pub user_id: String,
    pub status: PresenceStatus,
    pub last_seen: DateTime<Utc>,
}

/// An event published on the WebSocket hub whenever a message is posted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatBroadcast {
    pub event: String,
    pub channel_id: Uuid,
    pub message: ChatMessage,
}

// ---------------------------------------------------------------------------
// Live support types
// ---------------------------------------------------------------------------

/// Lifecycle of a live support session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SupportSessionStatus {
    Waiting,
    Active,
    Closed,
    Missed,
}

/// One live support session between a website visitor and an agent. The
/// conversation itself lives in the backing private channel
/// (`channel_id`); the session carries routing, queue, and outcome data.
/// `department_id` is an opaque identifier owned by the Departments
/// module — chat stores it and routes on it but never defines it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupportSession {
    pub id: Uuid,
    pub visitor_id: String,
    pub visitor_name: Option<String>,
    pub visitor_email: Option<String>,
    pub department_id: Option<String>,
    pub agent_id: Option<String>,
    pub status: SupportSessionStatus,
    pub subject: String,
    pub channel_id: Uuid,
    pub company_id: Option<String>,
    /// 1-based position among waiting sessions; 0 when not waiting.
    pub queue_position: u32,
    pub escalated_ticket_id: Option<String>,
    pub transfer_history: Vec<TransferRecord>,
    pub close_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
}

/// One agent-to-agent handover inside a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferRecord {
    pub from_agent_id: Option<String>,
    pub to_agent_id: String,
    pub note: String,
    pub transferred_by: String,
    pub created_at: DateTime<Utc>,
}

/// A support agent on the routing roster. `department_ids` lists the
/// departments the agent serves; `available` is the agent's own toggle.
/// Effective availability also requires a non-busy presence status (see
/// `agent_availability_rank`): an agent marked away still takes chats,
/// an offline or busy one does not.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupportAgent {
    pub user_id: String,
    pub department_ids: Vec<String>,
    pub available: bool,
    pub max_concurrent: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A reusable canned reply. Chat is the canonical store for canned
/// responses in this tree (the tickets module has none); other modules
/// reuse them through the list endpoint or the
/// `chat.canned_response.*` events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CannedResponse {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub category: String,
    pub department_id: Option<String>,
    pub created_by: String,
    pub usage_count: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A visitor's rating of a closed chat (1-5, plus an optional comment).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRating {
    pub id: Uuid,
    pub session_id: Uuid,
    pub visitor_id: String,
    pub agent_id: Option<String>,
    pub department_id: Option<String>,
    pub score: u8,
    pub comment: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// An offline message left by a visitor when no agent is available. It
/// is turned into a ticket through the `ticket.create_requested` hook;
/// `ticket_id` holds the created ticket when the tickets module
/// answered, and the message itself is kept for staff follow-up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfflineMessage {
    pub id: Uuid,
    pub visitor_name: String,
    pub visitor_email: String,
    pub department_id: Option<String>,
    pub subject: String,
    pub message: String,
    pub ticket_requested: bool,
    pub ticket_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Configuration for the embeddable website live-chat widget. One config
/// per company (keyed by `company_id`, or `"default"` when unset).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetConfig {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub name: String,
    pub greeting: String,
    pub offline_message: String,
    pub primary_color: String,
    pub departments: Vec<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A full chat transcript: the session, its messages oldest first, its
/// shared files, and its rating when one exists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTranscript {
    pub session: SupportSession,
    pub messages: Vec<ChatMessage>,
    pub files: Vec<ChatFile>,
    pub rating: Option<ChatRating>,
}

/// What a visitor (or the widget) gets back from bootstrap: the session,
/// the channel to talk on, the queue position, and whether an agent is
/// free right now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetBootstrap {
    pub session: SupportSession,
    pub channel_id: Uuid,
    pub queue_position: u32,
    pub agents_available: bool,
}

// ---------------------------------------------------------------------------
// WebSocket hub
// ---------------------------------------------------------------------------

/// The chat WebSocket hub: a tokio broadcast channel. Handlers publish
/// every new message here; WebSocket sessions subscribe and forward events
/// to their clients.
#[derive(Debug)]
pub struct ChatHub {
    sender: broadcast::Sender<ChatBroadcast>,
}

impl ChatHub {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(HUB_CAPACITY);
        Self { sender }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ChatBroadcast> {
        self.sender.subscribe()
    }

    pub fn publish(&self, event: ChatBroadcast) {
        let _ = self.sender.send(event);
    }
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateChannelRequest {
    pub name: String,
    pub kind: Option<ChannelKind>,
    pub company_id: Option<String>,
    pub members: Option<Vec<String>>,
    pub roles: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateChannelRequest {
    pub name: Option<String>,
    pub company_id: Option<Option<String>>,
    pub roles: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct AddMemberRequest {
    pub user_id: String,
}

#[derive(Debug, Deserialize)]
pub struct PostMessageRequest {
    pub body: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMessageRequest {
    pub body: String,
}

#[derive(Debug, Deserialize)]
pub struct ReactionRequest {
    pub emoji: String,
}

#[derive(Debug, Deserialize)]
pub struct EscalateRequest {
    pub title: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateDmRequest {
    pub user_id: String,
}

#[derive(Debug, Deserialize)]
pub struct PresenceRequest {
    pub status: PresenceStatus,
}

#[derive(Debug, Deserialize)]
pub struct ShareFileRequest {
    pub name: String,
    pub size: u64,
    pub mime: String,
    /// Storage target holding the bytes (see the storage module).
    pub storage_target: Option<String>,
    /// Object key inside the storage target.
    pub storage_key: Option<String>,
    /// Public or signed URL for the object, when the uploader has one.
    pub url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSupportSessionRequest {
    pub visitor_id: Option<String>,
    pub visitor_name: Option<String>,
    pub visitor_email: Option<String>,
    pub department_id: Option<String>,
    pub subject: Option<String>,
    pub company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CloseSessionRequest {
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TransferSessionRequest {
    pub to_agent_id: String,
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct EscalateSessionRequest {
    pub title: Option<String>,
    pub priority: Option<String>,
    pub category: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RateSessionRequest {
    pub score: u8,
    pub comment: Option<String>,
    pub visitor_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpsertAgentRequest {
    pub department_ids: Option<Vec<String>>,
    pub available: Option<bool>,
    pub max_concurrent: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAgentRequest {
    pub department_ids: Option<Vec<String>>,
    pub available: Option<bool>,
    pub max_concurrent: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct CreateCannedRequest {
    pub title: String,
    pub body: String,
    pub category: Option<String>,
    pub department_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCannedRequest {
    pub title: Option<String>,
    pub body: Option<String>,
    pub category: Option<String>,
    pub department_id: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct UseCannedRequest {
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateOfflineMessageRequest {
    pub visitor_name: String,
    pub visitor_email: String,
    pub department_id: Option<String>,
    pub subject: Option<String>,
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct UpsertWidgetConfigRequest {
    pub company_id: Option<String>,
    pub name: Option<String>,
    pub greeting: Option<String>,
    pub offline_message: Option<String>,
    pub primary_color: Option<String>,
    pub departments: Option<Vec<String>>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct WidgetConfigQuery {
    pub company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TranscriptQuery {
    pub format: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PresenceQuery {
    pub user_id: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct WsQuery {
    pub channel: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for chat. The hook bus is attached in `init` (the bus
/// lives in `AppContext`); handlers tolerate a missing bus by skipping
/// event emission, but still store and broadcast locally.
#[derive(Clone)]
pub struct ChatState {
    pub channels: Arc<DashMap<Uuid, Channel>>,
    pub messages: Arc<DashMap<Uuid, ChatMessage>>,
    pub files: Arc<DashMap<Uuid, ChatFile>>,
    pub presence: Arc<DashMap<String, PresenceEntry>>,
    pub hub: Arc<ChatHub>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
    /// Live support sessions, keyed by session id.
    pub sessions: Arc<DashMap<Uuid, SupportSession>>,
    /// Support agent roster, keyed by agent user id.
    pub agents: Arc<DashMap<String, SupportAgent>>,
    /// Canned responses, keyed by canned response id.
    pub canned: Arc<DashMap<Uuid, CannedResponse>>,
    /// Chat ratings, keyed by rating id.
    pub ratings: Arc<DashMap<Uuid, ChatRating>>,
    /// Offline messages, keyed by message id.
    pub offline_messages: Arc<DashMap<Uuid, OfflineMessage>>,
    /// Widget configs, keyed by company id or `"default"`.
    pub widget_configs: Arc<DashMap<String, WidgetConfig>>,
}

impl Default for ChatState {
    fn default() -> Self {
        Self {
            channels: Arc::new(DashMap::new()),
            messages: Arc::new(DashMap::new()),
            files: Arc::new(DashMap::new()),
            presence: Arc::new(DashMap::new()),
            hub: Arc::new(ChatHub::new()),
            hooks: Arc::new(RwLock::new(None)),
            sessions: Arc::new(DashMap::new()),
            agents: Arc::new(DashMap::new()),
            canned: Arc::new(DashMap::new()),
            ratings: Arc::new(DashMap::new()),
            offline_messages: Arc::new(DashMap::new()),
            widget_configs: Arc::new(DashMap::new()),
        }
    }
}

impl ChatState {
    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.hooks.read().expect("hooks lock poisoned").clone() {
            bus.emit(event, data);
        }
    }

    /// Emit an event and return the final hook context, so callers can
    /// read handler results (for example the ticket created by the
    /// tickets module in answer to `ticket.create_requested`). With no
    /// bus attached, returns an empty context for the event.
    fn emit_ctx(&self, event: &str, data: Value) -> HookContext {
        match self.hooks.read().expect("hooks lock poisoned").clone() {
            Some(bus) => bus.emit(event, data),
            None => HookContext::new(event, data),
        }
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

/// Roles claimed by the caller via the interim `X-User-Roles` header.
fn caller_roles(req: &HttpRequest) -> Vec<String> {
    req.headers()
        .get("x-user-roles")
        .and_then(|value| value.to_str().ok())
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|role| !role.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
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

fn clean_members(members: Option<Vec<String>>, owner: &str) -> UnifiedResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for member in members.unwrap_or_default() {
        let cleaned = require_text(&member, "member user id", 128)?;
        if !out.contains(&cleaned) {
            out.push(cleaned);
        }
    }
    if !out.contains(&owner.to_string()) {
        out.push(owner.to_string());
    }
    if out.len() > MAX_MEMBERS {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_MEMBERS} members are allowed"
        )));
    }
    Ok(out)
}

fn clean_roles(roles: Option<Vec<String>>) -> UnifiedResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for role in roles.unwrap_or_default() {
        let cleaned = require_text(&role, "role", 64)?;
        if !out.contains(&cleaned) {
            out.push(cleaned);
        }
    }
    Ok(out)
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

/// True when the caller may see (and post in) the channel: members always,
/// everyone for unrestricted public channels, role holders for restricted
/// public channels, nobody else for private and DM channels.
fn can_view(channel: &Channel, user: &str, roles: &[String]) -> bool {
    if channel.members.iter().any(|member| member == user) {
        return true;
    }
    match channel.kind {
        ChannelKind::Public => {
            if channel.roles.is_empty() {
                true
            } else {
                channel.roles.iter().any(|role| roles.contains(role))
            }
        }
        ChannelKind::Private | ChannelKind::Dm => false,
    }
}

fn load_channel(state: &ChatState, raw_id: &str) -> UnifiedResult<Channel> {
    let id = parse_uuid(raw_id, "channel")?;
    state
        .channels
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Channel not found: {id}")))
}

fn load_message(state: &ChatState, channel_id: Uuid, raw_id: &str) -> UnifiedResult<ChatMessage> {
    let id = parse_uuid(raw_id, "message")?;
    let message = state
        .messages
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Message not found: {id}")))?;
    if message.channel_id != channel_id {
        return Err(UnifiedError::not_found(format!("Message not found: {id}")));
    }
    Ok(message)
}

/// Find `@user` mentions in a message body. The captured name is treated
/// as a user id; the notifications module resolves it.
fn find_mentions(body: &str) -> Vec<String> {
    let pattern = Regex::new(r"@([A-Za-z0-9][A-Za-z0-9_-]*)").expect("mention regex is valid");
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for capture in pattern.captures_iter(body) {
        let name = capture[1].to_string();
        if seen.insert(name.clone()) {
            out.push(name);
        }
    }
    out
}

fn require_email(value: &str) -> UnifiedResult<String> {
    let cleaned = require_text(value, "email", MAX_EMAIL_LEN)?;
    if !cleaned.contains('@') || !cleaned.contains('.') {
        return Err(UnifiedError::bad_request("email must be a valid email address"));
    }
    Ok(cleaned)
}

fn clean_department_ids(departments: Option<Vec<String>>) -> UnifiedResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for department in departments.unwrap_or_default() {
        let cleaned = require_text(&department, "department_id", MAX_DEPARTMENT_LEN)?;
        if !out.contains(&cleaned) {
            out.push(cleaned);
        }
    }
    Ok(out)
}

fn clean_optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Ok(Some(require_text(raw, field, max)?)),
        None => Ok(None),
    }
}

fn valid_ticket_priority(priority: &str) -> bool {
    matches!(priority, "low" | "normal" | "high" | "urgent")
}

fn load_session(state: &ChatState, raw_id: &str) -> UnifiedResult<SupportSession> {
    let id = parse_uuid(raw_id, "session")?;
    state
        .sessions
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Support session not found: {id}")))
}

/// Recompute the 1-based queue position of every waiting session
/// (oldest waiting visitor first). Sessions that are not waiting get 0.
fn refresh_queue_positions(state: &ChatState) {
    let mut waiting: Vec<SupportSession> = state
        .sessions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|session| session.status == SupportSessionStatus::Waiting)
        .collect();
    waiting.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
    let mut positions: std::collections::HashMap<Uuid, u32> = std::collections::HashMap::new();
    for (index, session) in waiting.iter().enumerate() {
        positions.insert(session.id, (index + 1) as u32);
    }
    // Snapshot current positions first, then write, so iteration and
    // mutation of the same map never overlap.
    let current: Vec<(Uuid, u32)> = state
        .sessions
        .iter()
        .map(|entry| (entry.value().id, entry.value().queue_position))
        .collect();
    let updates: Vec<(Uuid, u32)> = current
        .into_iter()
        .map(|(id, current_position)| {
            (id, positions.get(&id).copied().unwrap_or(0), current_position)
        })
        .filter(|(_, position, current_position)| position != current_position)
        .map(|(id, position, _)| (id, position))
        .collect();
    for (id, position) in updates {
        if let Some(mut entry) = state.sessions.get_mut(&id) {
            entry.queue_position = position;
        }
    }
}

/// Effective availability of a roster agent. `None` means the agent
/// cannot take chats right now; `Some(rank)` ranks candidates for
/// routing (online first, then agents with no presence entry, then away).
/// An offline or busy presence always wins over the agent's own toggle.
fn agent_availability_rank(state: &ChatState, agent: &SupportAgent) -> Option<u8> {
    if !agent.available {
        return None;
    }
    presence_rank(state, &agent.user_id)
}

/// Presence-only availability rank for a user id: online first, then
/// users with no presence entry, then away. Offline or busy means
/// unavailable.
fn presence_rank(state: &ChatState, user_id: &str) -> Option<u8> {
    match state.presence.get(user_id) {
        Some(entry) => match entry.status {
            PresenceStatus::Online => Some(0),
            PresenceStatus::Away => Some(2),
            PresenceStatus::Busy | PresenceStatus::Offline => None,
        },
        None => Some(1),
    }
}

/// Ask the Departments module for a department's agent user ids
/// (`department.agents_requested`, see `tickets.rs`,
/// `agents_for_department`). Empty when the module is not registered.
fn department_agent_ids(state: &ChatState, department_id: &str) -> Vec<String> {
    let context = state.emit_ctx(
        "department.agents_requested",
        json!({ "department_id": department_id }),
    );
    for result in &context.results {
        if let Some(ids) = result.get("agent_ids").and_then(|value| value.as_array()) {
            return ids
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect();
        }
    }
    Vec::new()
}

/// The best agent for a department: first the agents the Departments
/// module reports for it (roster record supplies the availability
/// toggle and concurrent-chat limit; agents without a chat roster
/// record count as available with the default limit), then — when the
/// Departments module does not answer — chat's own roster for the
/// department. `department_id` of `None` matches any roster agent.
/// Fewest active chats first, then best presence rank.
fn best_available_agent(state: &ChatState, department_id: Option<&str>) -> Option<String> {
    if let Some(department) = department_id {
        let mut scored: Vec<(u64, u8, String)> = Vec::new();
        for agent_id in department_agent_ids(state, department) {
            let (available, max_concurrent) = match state.agents.get(&agent_id) {
                Some(entry) => (entry.available, entry.max_concurrent),
                None => (true, DEFAULT_MAX_CONCURRENT_CHATS),
            };
            if !available {
                continue;
            }
            let Some(rank) = presence_rank(state, &agent_id) else {
                continue;
            };
            let active = active_chat_count(state, &agent_id);
            if active >= max_concurrent as u64 {
                continue;
            }
            scored.push((active, rank, agent_id));
        }
        if !scored.is_empty() {
            scored.sort();
            return scored.into_iter().next().map(|(_, _, user_id)| user_id);
        }
    }
    let mut candidates: Vec<(u64, u8, String)> = state
        .agents
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|agent| match department_id {
            Some(department) => agent.department_ids.iter().any(|id| id == department),
            None => true,
        })
        .filter_map(|agent| {
            let rank = agent_availability_rank(state, &agent)?;
            let active = active_chat_count(state, &agent.user_id);
            if active >= agent.max_concurrent as u64 {
                return None;
            }
            Some((active, rank, agent.user_id))
        })
        .collect();
    candidates.sort();
    candidates.into_iter().next().map(|(_, _, user_id)| user_id)
}

fn agents_available(state: &ChatState, department_id: Option<&str>) -> bool {
    best_available_agent(state, department_id).is_some()
}

/// Add the assigned agent to the session's backing channel so they can
/// read and post in it.
fn add_agent_to_channel(state: &ChatState, session: &SupportSession) {
    let Some(agent_id) = &session.agent_id else {
        return;
    };
    if let Some(mut channel) = state.channels.get_mut(&session.channel_id) {
        if !channel.members.contains(agent_id) {
            channel.members.push(agent_id.clone());
            channel.updated_at = Utc::now();
        }
    }
}

/// Number of active sessions currently assigned to an agent.
fn active_chat_count(state: &ChatState, agent_id: &str) -> u64 {
    state
        .sessions
        .iter()
        .filter(|entry| {
            let session = entry.value();
            session.status == SupportSessionStatus::Active
                && session.agent_id.as_deref() == Some(agent_id)
        })
        .count() as u64
}

/// Route a session to a department agent (`best_available_agent` asks
/// the Departments module first and falls back to chat's own roster).
/// With no agent free the session waits in the queue.
fn route_session(state: &ChatState, session: &mut SupportSession) {
    let agent_id = best_available_agent(state, session.department_id.as_deref());
    match agent_id {
        Some(agent_id) => {
            session.agent_id = Some(agent_id.clone());
            session.status = SupportSessionStatus::Active;
            session.updated_at = Utc::now();
            add_agent_to_channel(state, session);
            state.emit(
                "chat.session.assigned",
                json!({
                    "session_id": session.id.to_string(),
                    "agent_id": agent_id,
                    "department_id": session.department_id.clone(),
                    "visitor_id": session.visitor_id.clone(),
                }),
            );
        }
        None => {
            session.status = SupportSessionStatus::Waiting;
            session.updated_at = Utc::now();
            state.emit(
                "chat.session.queued",
                json!({
                    "session_id": session.id.to_string(),
                    "department_id": session.department_id.clone(),
                    "visitor_id": session.visitor_id.clone(),
                }),
            );
        }
    }
}

/// Create the backing private channel and the session, then route it.
/// Shared by the widget bootstrap and staff-created sessions.
fn create_support_session_inner(
    state: &ChatState,
    visitor_id: String,
    visitor_name: Option<String>,
    visitor_email: Option<String>,
    department_id: Option<String>,
    subject: String,
    company_id: Option<String>,
) -> UnifiedResult<SupportSession> {
    let session_id = Uuid::now_v7();
    let now = Utc::now();
    let channel = Channel {
        id: Uuid::now_v7(),
        name: format!("support:{session_id}"),
        kind: ChannelKind::Private,
        company_id: company_id.clone(),
        owner_id: visitor_id.clone(),
        members: vec![visitor_id.clone()],
        roles: Vec::new(),
        created_at: now,
        updated_at: now,
    };
    state.channels.insert(channel.id, channel.clone());
    let mut session = SupportSession {
        id: session_id,
        visitor_id: visitor_id.clone(),
        visitor_name,
        visitor_email,
        department_id,
        agent_id: None,
        status: SupportSessionStatus::Waiting,
        subject,
        channel_id: channel.id,
        company_id,
        queue_position: 0,
        escalated_ticket_id: None,
        transfer_history: Vec::new(),
        close_reason: None,
        created_at: now,
        updated_at: now,
        closed_at: None,
    };
    route_session(state, &mut session);
    state.sessions.insert(session.id, session.clone());
    refresh_queue_positions(state);
    let stored = state
        .sessions
        .get(&session.id)
        .map(|entry| entry.value().clone())
        .unwrap_or(session);
    state.emit(
        "chat.session.created",
        json!({
            "session_id": stored.id.to_string(),
            "visitor_id": stored.visitor_id.clone(),
            "department_id": stored.department_id.clone(),
            "agent_id": stored.agent_id.clone(),
            "status": format!("{:?}", stored.status).to_lowercase(),
            "channel_id": stored.channel_id.to_string(),
            "company_id": stored.company_id.clone(),
        }),
    );
    Ok(stored)
}

/// Post a system message into a channel (transfer notices, queue notes).
/// System messages are not attributed to a user.
fn post_system_message(state: &ChatState, channel_id: Uuid, body: String) {
    let now = Utc::now();
    let message = ChatMessage {
        id: Uuid::now_v7(),
        channel_id,
        author_id: "system".to_string(),
        body,
        edited: false,
        reactions: Vec::new(),
        created_at: now,
        updated_at: now,
    };
    state.messages.insert(message.id, message.clone());
    state.hub.publish(ChatBroadcast {
        event: "chat.message".to_string(),
        channel_id,
        message,
    });
}

/// Render a session as plain text for ticket bodies and exports.
fn build_transcript_text(state: &ChatState, session: &SupportSession) -> String {
    let mut lines: Vec<String> = vec![
        "Live chat transcript".to_string(),
        format!("Session: {}", session.id),
        format!("Visitor: {}", session.visitor_id),
    ];
    if let Some(name) = &session.visitor_name {
        lines.push(format!("Visitor name: {name}"));
    }
    if let Some(email) = &session.visitor_email {
        lines.push(format!("Visitor email: {email}"));
    }
    if let Some(department) = &session.department_id {
        lines.push(format!("Department: {department}"));
    }
    if let Some(agent) = &session.agent_id {
        lines.push(format!("Agent: {agent}"));
    }
    lines.push(format!("Subject: {}", session.subject));
    lines.push(format!("Started: {}", session.created_at.to_rfc3339()));
    if let Some(closed) = session.closed_at {
        lines.push(format!("Closed: {}", closed.to_rfc3339()));
    }
    lines.push(String::new());
    let mut messages: Vec<ChatMessage> = state
        .messages
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|message| message.channel_id == session.channel_id)
        .collect();
    messages.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
    for message in &messages {
        lines.push(format!(
            "[{}] {}: {}",
            message.created_at.to_rfc3339(),
            message.author_id,
            message.body
        ));
    }
    let files: Vec<ChatFile> = state
        .files
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|file| file.channel_id == session.channel_id)
        .collect();
    if !files.is_empty() {
        lines.push(String::new());
        lines.push("Shared files:".to_string());
        for file in &files {
            let reference = file
                .storage_key
                .clone()
                .or_else(|| file.url.clone())
                .unwrap_or_default();
            lines.push(format!(
                "- {} ({} bytes, {}) {}",
                file.name, file.size, file.mime, reference
            ));
        }
    }
    if !session.transfer_history.is_empty() {
        lines.push(String::new());
        lines.push("Transfers:".to_string());
        for transfer in &session.transfer_history {
            lines.push(format!(
                "- {} -> {} (by {}, {}): {}",
                transfer.from_agent_id.as_deref().unwrap_or("unassigned"),
                transfer.to_agent_id,
                transfer.transferred_by,
                transfer.created_at.to_rfc3339(),
                transfer.note
            ));
        }
    }
    lines.join("\n")
}

/// Emit `ticket.create_requested` and read the answer. The tickets
/// module's handler (`tickets.rs`, `on_ticket_create_requested`) adds the
/// created ticket as a result, or `{"error": ...}` when validation
/// failed. Returns the created ticket id, or `None` when no tickets
/// handler answered (ticket creation is best-effort from chat's side).
fn request_ticket(state: &ChatState, payload: Value) -> UnifiedResult<Option<String>> {
    let context = state.emit_ctx("ticket.create_requested", payload);
    for result in &context.results {
        if let Some(error) = result.get("error").and_then(|value| value.as_str()) {
            return Err(UnifiedError::unprocessable(format!(
                "Ticket creation failed: {error}"
            )));
        }
        if let Some(id) = result.get("id").and_then(|value| value.as_str()) {
            return Ok(Some(id.to_string()));
        }
    }
    Ok(None)
}

fn default_widget_config(company_id: Option<String>) -> WidgetConfig {
    let now = Utc::now();
    WidgetConfig {
        id: Uuid::nil(),
        company_id,
        name: "Live chat".to_string(),
        greeting: "Hello! How can we help you today?".to_string(),
        offline_message: "No agents are available right now. Leave a message and we will get back to you.".to_string(),
        primary_color: "#0B6B3A".to_string(),
        departments: Vec::new(),
        enabled: true,
        created_at: now,
        updated_at: now,
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct ChatModule {
    state: ChatState,
}

impl ChatModule {
    pub fn new() -> Self {
        Self {
            state: ChatState::default(),
        }
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ChatModule::new()));
}

impl Module for ChatModule {
    fn id(&self) -> &'static str {
        "chat"
    }

    fn name(&self) -> &'static str {
        "Chat"
    }

    fn description(&self) -> &'static str {
        "Real-time messaging: channels, direct messages, reactions, file sharing, presence, a WebSocket hub, and live support chat with widget, department routing, queues, canned responses, transcripts, ratings, transfers, and ticket escalation."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "company", "notifications", "tickets", "storage"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/chat/channels", owner: "chat", description: "List channels" },
            RouteInfo { method: "POST", path: "/api/v1/chat/channels", owner: "chat", description: "Create channel" },
            RouteInfo { method: "GET", path: "/api/v1/chat/channels/{id}", owner: "chat", description: "Get channel" },
            RouteInfo { method: "PATCH", path: "/api/v1/chat/channels/{id}", owner: "chat", description: "Update channel" },
            RouteInfo { method: "DELETE", path: "/api/v1/chat/channels/{id}", owner: "chat", description: "Delete channel" },
            RouteInfo { method: "POST", path: "/api/v1/chat/channels/{id}/members", owner: "chat", description: "Add member" },
            RouteInfo { method: "DELETE", path: "/api/v1/chat/channels/{id}/members/{user_id}", owner: "chat", description: "Remove member" },
            RouteInfo { method: "GET", path: "/api/v1/chat/channels/{id}/messages", owner: "chat", description: "List messages" },
            RouteInfo { method: "POST", path: "/api/v1/chat/channels/{id}/messages", owner: "chat", description: "Post message" },
            RouteInfo { method: "PATCH", path: "/api/v1/chat/channels/{id}/messages/{message_id}", owner: "chat", description: "Update message" },
            RouteInfo { method: "DELETE", path: "/api/v1/chat/channels/{id}/messages/{message_id}", owner: "chat", description: "Delete message" },
            RouteInfo { method: "POST", path: "/api/v1/chat/channels/{id}/messages/{message_id}/reactions", owner: "chat", description: "Toggle reaction" },
            RouteInfo { method: "POST", path: "/api/v1/chat/channels/{id}/messages/{message_id}/escalate", owner: "chat", description: "Escalate message" },
            RouteInfo { method: "GET", path: "/api/v1/chat/channels/{id}/files", owner: "chat", description: "List files" },
            RouteInfo { method: "POST", path: "/api/v1/chat/channels/{id}/files", owner: "chat", description: "Share file" },
            RouteInfo { method: "GET", path: "/api/v1/chat/dm", owner: "chat", description: "List dms" },
            RouteInfo { method: "POST", path: "/api/v1/chat/dm", owner: "chat", description: "Create dm" },
            RouteInfo { method: "GET", path: "/api/v1/chat/presence", owner: "chat", description: "List presence" },
            RouteInfo { method: "POST", path: "/api/v1/chat/presence", owner: "chat", description: "Set presence" },
            RouteInfo { method: "GET", path: "/api/v1/chat/ws", owner: "chat", description: "Chat ws" },
            RouteInfo { method: "GET", path: "/api/v1/chat/support/widget/config", owner: "chat", description: "Get widget config" },
            RouteInfo { method: "PUT", path: "/api/v1/chat/support/widget/config", owner: "chat", description: "Save widget config" },
            RouteInfo { method: "POST", path: "/api/v1/chat/support/widget/sessions", owner: "chat", description: "Bootstrap widget session" },
            RouteInfo { method: "GET", path: "/api/v1/chat/support/widget/sessions/{id}", owner: "chat", description: "Poll widget session" },
            RouteInfo { method: "GET", path: "/api/v1/chat/support/sessions", owner: "chat", description: "List support sessions" },
            RouteInfo { method: "POST", path: "/api/v1/chat/support/sessions", owner: "chat", description: "Create support session" },
            RouteInfo { method: "GET", path: "/api/v1/chat/support/sessions/{id}", owner: "chat", description: "Get support session" },
            RouteInfo { method: "POST", path: "/api/v1/chat/support/sessions/{id}/accept", owner: "chat", description: "Accept support session" },
            RouteInfo { method: "POST", path: "/api/v1/chat/support/sessions/{id}/close", owner: "chat", description: "Close support session" },
            RouteInfo { method: "POST", path: "/api/v1/chat/support/sessions/{id}/transfer", owner: "chat", description: "Transfer support session" },
            RouteInfo { method: "POST", path: "/api/v1/chat/support/sessions/{id}/escalate", owner: "chat", description: "Escalate session to ticket" },
            RouteInfo { method: "GET", path: "/api/v1/chat/support/sessions/{id}/transcript", owner: "chat", description: "Get session transcript" },
            RouteInfo { method: "GET", path: "/api/v1/chat/support/sessions/{id}/position", owner: "chat", description: "Get queue position" },
            RouteInfo { method: "POST", path: "/api/v1/chat/support/sessions/{id}/rating", owner: "chat", description: "Rate support session" },
            RouteInfo { method: "GET", path: "/api/v1/chat/support/sessions/{id}/rating", owner: "chat", description: "Get session rating" },
            RouteInfo { method: "GET", path: "/api/v1/chat/support/ratings", owner: "chat", description: "List chat ratings" },
            RouteInfo { method: "GET", path: "/api/v1/chat/support/queue", owner: "chat", description: "List waiting visitors" },
            RouteInfo { method: "GET", path: "/api/v1/chat/support/agents", owner: "chat", description: "List support agents" },
            RouteInfo { method: "POST", path: "/api/v1/chat/support/agents", owner: "chat", description: "Register support agent" },
            RouteInfo { method: "GET", path: "/api/v1/chat/support/agents/{user_id}", owner: "chat", description: "Get support agent" },
            RouteInfo { method: "PATCH", path: "/api/v1/chat/support/agents/{user_id}", owner: "chat", description: "Update support agent" },
            RouteInfo { method: "DELETE", path: "/api/v1/chat/support/agents/{user_id}", owner: "chat", description: "Remove support agent" },
            RouteInfo { method: "GET", path: "/api/v1/chat/support/canned", owner: "chat", description: "List canned responses" },
            RouteInfo { method: "POST", path: "/api/v1/chat/support/canned", owner: "chat", description: "Create canned response" },
            RouteInfo { method: "GET", path: "/api/v1/chat/support/canned/{id}", owner: "chat", description: "Get canned response" },
            RouteInfo { method: "PATCH", path: "/api/v1/chat/support/canned/{id}", owner: "chat", description: "Update canned response" },
            RouteInfo { method: "DELETE", path: "/api/v1/chat/support/canned/{id}", owner: "chat", description: "Delete canned response" },
            RouteInfo { method: "POST", path: "/api/v1/chat/support/canned/{id}/use", owner: "chat", description: "Use canned response" },
            RouteInfo { method: "GET", path: "/api/v1/chat/support/offline-messages", owner: "chat", description: "List offline messages" },
            RouteInfo { method: "POST", path: "/api/v1/chat/support/offline-messages", owner: "chat", description: "Create offline message" },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/chat")
                .app_data(state)
                .route("/channels", web::get().to(list_channels))
                .route("/channels", web::post().to(create_channel))
                .route("/channels/{id}", web::get().to(get_channel))
                .route("/channels/{id}", web::patch().to(update_channel))
                .route("/channels/{id}", web::delete().to(delete_channel))
                .route("/channels/{id}/members", web::post().to(add_member))
                .route(
                    "/channels/{id}/members/{user_id}",
                    web::delete().to(remove_member),
                )
                .route("/channels/{id}/messages", web::get().to(list_messages))
                .route("/channels/{id}/messages", web::post().to(post_message))
                .route(
                    "/channels/{id}/messages/{message_id}",
                    web::patch().to(update_message),
                )
                .route(
                    "/channels/{id}/messages/{message_id}",
                    web::delete().to(delete_message),
                )
                .route(
                    "/channels/{id}/messages/{message_id}/reactions",
                    web::post().to(toggle_reaction),
                )
                .route(
                    "/channels/{id}/messages/{message_id}/escalate",
                    web::post().to(escalate_message),
                )
                .route("/channels/{id}/files", web::get().to(list_files))
                .route("/channels/{id}/files", web::post().to(share_file))
                .route("/dm", web::get().to(list_dms))
                .route("/dm", web::post().to(create_dm))
                .route("/presence", web::get().to(list_presence))
                .route("/presence", web::post().to(set_presence))
                .route("/ws", web::get().to(chat_ws))
                .route("/support/widget/config", web::get().to(get_widget_config))
                .route("/support/widget/config", web::put().to(save_widget_config))
                .route(
                    "/support/widget/sessions",
                    web::post().to(bootstrap_widget_session),
                )
                .route(
                    "/support/widget/sessions/{id}",
                    web::get().to(poll_widget_session),
                )
                .route("/support/sessions", web::get().to(list_support_sessions))
                .route("/support/sessions", web::post().to(create_support_session))
                .route("/support/sessions/{id}", web::get().to(get_support_session))
                .route(
                    "/support/sessions/{id}/accept",
                    web::post().to(accept_support_session),
                )
                .route(
                    "/support/sessions/{id}/close",
                    web::post().to(close_support_session),
                )
                .route(
                    "/support/sessions/{id}/transfer",
                    web::post().to(transfer_support_session),
                )
                .route(
                    "/support/sessions/{id}/escalate",
                    web::post().to(escalate_support_session),
                )
                .route(
                    "/support/sessions/{id}/transcript",
                    web::get().to(get_session_transcript),
                )
                .route(
                    "/support/sessions/{id}/position",
                    web::get().to(get_queue_position),
                )
                .route(
                    "/support/sessions/{id}/rating",
                    web::post().to(rate_support_session),
                )
                .route(
                    "/support/sessions/{id}/rating",
                    web::get().to(get_session_rating),
                )
                .route("/support/ratings", web::get().to(list_ratings))
                .route("/support/queue", web::get().to(list_queue))
                .route("/support/agents", web::get().to(list_agents))
                .route("/support/agents", web::post().to(register_agent))
                .route("/support/agents/{user_id}", web::get().to(get_agent))
                .route("/support/agents/{user_id}", web::patch().to(update_agent))
                .route("/support/agents/{user_id}", web::delete().to(remove_agent))
                .route("/support/canned", web::get().to(list_canned))
                .route("/support/canned", web::post().to(create_canned))
                .route("/support/canned/{id}", web::get().to(get_canned))
                .route("/support/canned/{id}", web::patch().to(update_canned))
                .route("/support/canned/{id}", web::delete().to(delete_canned))
                .route("/support/canned/{id}/use", web::post().to(use_canned))
                .route(
                    "/support/offline-messages",
                    web::get().to(list_offline_messages),
                )
                .route(
                    "/support/offline-messages",
                    web::post().to(create_offline_message),
                ),
        );
    }
}

// ---------------------------------------------------------------------------
// Channel handlers
// ---------------------------------------------------------------------------

async fn create_channel(
    state: web::Data<ChatState>,
    req: HttpRequest,
    body: web::Json<CreateChannelRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Channel> = (|| {
        let actor = actor_id(&req)?;
        let kind = body.kind.unwrap_or_default();
        if kind == ChannelKind::Dm {
            return Err(UnifiedError::bad_request(
                "Direct message channels must be created through POST /api/v1/chat/dm",
            ));
        }
        let name = require_text(&body.name, "name", MAX_NAME_LEN)?;
        let company_id = match &body.company_id {
            Some(raw) => Some(require_text(raw, "company_id", 128)?),
            None => None,
        };
        let members = clean_members(body.members.clone(), &actor)?;
        let roles = clean_roles(body.roles.clone())?;
        let now = Utc::now();
        let channel = Channel {
            id: Uuid::now_v7(),
            name,
            kind,
            company_id,
            owner_id: actor.clone(),
            members,
            roles,
            created_at: now,
            updated_at: now,
        };
        state.channels.insert(channel.id, channel.clone());
        state.emit(
            "chat.channel.created",
            json!({
                "channel_id": channel.id.to_string(),
                "name": channel.name.clone(),
                "kind": format!("{:?}", channel.kind).to_lowercase(),
                "owner_id": actor,
                "company_id": channel.company_id.clone(),
            }),
        );
        Ok(channel)
    })();
    match result {
        Ok(channel) => created(channel),
        Err(error) => err(error),
    }
}

async fn list_channels(
    state: web::Data<ChatState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<Channel>> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Channel> = state
            .channels
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|channel| can_view(channel, &actor, &roles))
            .filter(|channel| {
                if let Some(kind) = &kind_filter {
                    let expected = match kind.as_str() {
                        "public" => ChannelKind::Public,
                        "private" => ChannelKind::Private,
                        "dm" => ChannelKind::Dm,
                        _ => return false,
                    };
                    if channel.kind != expected {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if channel.company_id.as_deref() != Some(company_id.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() && !channel.name.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["name", "created_at", "updated_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.cmp(&b.name),
                "updated_at" => a.updated_at.cmp(&b.updated_at),
                _ => a.created_at.cmp(&b.created_at),
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

async fn get_channel(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Channel> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let channel = load_channel(&state, &path.into_inner())?;
        if !can_view(&channel, &actor, &roles) {
            return Err(UnifiedError::forbidden("Channel is not visible to this user"));
        }
        Ok(channel)
    })();
    match result {
        Ok(channel) => ok(channel),
        Err(error) => err(error),
    }
}

async fn update_channel(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateChannelRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Channel> = (|| {
        let actor = actor_id(&req)?;
        let mut channel = load_channel(&state, &path.into_inner())?;
        if channel.owner_id != actor {
            return Err(UnifiedError::forbidden(
                "Only the channel owner can update the channel",
            ));
        }
        if let Some(name) = &body.name {
            channel.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(company_id) = &body.company_id {
            channel.company_id = match company_id {
                Some(raw) => Some(require_text(raw, "company_id", 128)?),
                None => None,
            };
        }
        if let Some(roles) = &body.roles {
            channel.roles = clean_roles(Some(roles.clone()))?;
        }
        channel.updated_at = Utc::now();
        state.channels.insert(channel.id, channel.clone());
        state.emit(
            "chat.channel.updated",
            json!({ "channel_id": channel.id.to_string(), "updated_by": actor }),
        );
        Ok(channel)
    })();
    match result {
        Ok(channel) => ok(channel),
        Err(error) => err(error),
    }
}

async fn delete_channel(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let actor = actor_id(&req)?;
        let channel = load_channel(&state, &path.into_inner())?;
        if channel.owner_id != actor {
            return Err(UnifiedError::forbidden(
                "Only the channel owner can delete the channel",
            ));
        }
        state.channels.remove(&channel.id);
        state.messages.retain(|_, message| message.channel_id != channel.id);
        state.files.retain(|_, file| file.channel_id != channel.id);
        state.emit(
            "chat.channel.deleted",
            json!({ "channel_id": channel.id.to_string(), "deleted_by": actor }),
        );
        Ok(json!({ "deleted": true, "channel_id": channel.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}

async fn add_member(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddMemberRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Channel> = (|| {
        let actor = actor_id(&req)?;
        let mut channel = load_channel(&state, &path.into_inner())?;
        if channel.owner_id != actor {
            return Err(UnifiedError::forbidden(
                "Only the channel owner can add members",
            ));
        }
        let user_id = require_text(&body.user_id, "user_id", 128)?;
        if channel.members.contains(&user_id) {
            return Err(UnifiedError::conflict(format!(
                "User {user_id} is already a member of this channel"
            )));
        }
        if channel.members.len() >= MAX_MEMBERS {
            return Err(UnifiedError::bad_request(format!(
                "At most {MAX_MEMBERS} members are allowed"
            )));
        }
        channel.members.push(user_id);
        channel.updated_at = Utc::now();
        state.channels.insert(channel.id, channel.clone());
        Ok(channel)
    })();
    match result {
        Ok(channel) => ok(channel),
        Err(error) => err(error),
    }
}

async fn remove_member(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Channel> = (|| {
        let actor = actor_id(&req)?;
        let (raw_id, user_id) = path.into_inner();
        let mut channel = load_channel(&state, &raw_id)?;
        if channel.owner_id != actor && actor != user_id {
            return Err(UnifiedError::forbidden(
                "Only the channel owner or the member themselves can remove a member",
            ));
        }
        if channel.owner_id == user_id {
            return Err(UnifiedError::bad_request(
                "The channel owner cannot be removed; delete the channel instead",
            ));
        }
        let before = channel.members.len();
        channel.members.retain(|member| member != &user_id);
        if channel.members.len() == before {
            return Err(UnifiedError::not_found(format!(
                "User {user_id} is not a member of this channel"
            )));
        }
        channel.updated_at = Utc::now();
        state.channels.insert(channel.id, channel.clone());
        Ok(channel)
    })();
    match result {
        Ok(channel) => ok(channel),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Message handlers
// ---------------------------------------------------------------------------

async fn post_message(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PostMessageRequest>,
) -> HttpResponse {
    let result: UnifiedResult<ChatMessage> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let channel = load_channel(&state, &path.into_inner())?;
        if !can_view(&channel, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "This user cannot post in this channel",
            ));
        }
        let text = require_text(&body.body, "body", MAX_BODY_LEN)?;
        let now = Utc::now();
        let message = ChatMessage {
            id: Uuid::now_v7(),
            channel_id: channel.id,
            author_id: actor.clone(),
            body: text.clone(),
            edited: false,
            reactions: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        state.messages.insert(message.id, message.clone());
        // Broadcast to the WebSocket hub first so live clients see the
        // message even if a hook handler fails further down.
        state.hub.publish(ChatBroadcast {
            event: "chat.message".to_string(),
            channel_id: channel.id,
            message: message.clone(),
        });
        state.emit(
            "chat.message",
            json!({
                "channel_id": channel.id.to_string(),
                "message_id": message.id.to_string(),
                "author_id": actor.clone(),
                "body": text.clone(),
                "company_id": channel.company_id,
            }),
        );
        // One notification per @user mention (excluding self-mentions).
        for mentioned in find_mentions(&text) {
            if mentioned == actor {
                continue;
            }
            state.emit(
                "notification.requested",
                json!({
                    "user_id": mentioned,
                    "kind": "chat.mention",
                    "channel_id": channel.id.to_string(),
                    "message_id": message.id.to_string(),
                    "author_id": actor.clone(),
                    "excerpt": text.chars().take(200).collect::<String>(),
                }),
            );
        }
        Ok(message)
    })();
    match result {
        Ok(message) => created(message),
        Err(error) => err(error),
    }
}

async fn list_messages(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<ChatMessage>> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let channel = load_channel(&state, &path.into_inner())?;
        if !can_view(&channel, &actor, &roles) {
            return Err(UnifiedError::forbidden("Channel is not visible to this user"));
        }
        let params = query.into_inner();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let author_filter = params.filters.get("author_id").cloned();
        let mut items: Vec<ChatMessage> = state
            .messages
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|message| message.channel_id == channel.id)
            .filter(|message| {
                if let Some(author) = &author_filter {
                    if &message.author_id != author {
                        return false;
                    }
                }
                if !search.is_empty() && !message.body.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        // Newest first.
        items.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

async fn update_message(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateMessageRequest>,
) -> HttpResponse {
    let result: UnifiedResult<ChatMessage> = (|| {
        let actor = actor_id(&req)?;
        let (raw_channel, raw_message) = path.into_inner();
        let channel = load_channel(&state, &raw_channel)?;
        let mut message = load_message(&state, channel.id, &raw_message)?;
        if message.author_id != actor {
            return Err(UnifiedError::forbidden(
                "Only the message author can edit the message",
            ));
        }
        message.body = require_text(&body.body, "body", MAX_BODY_LEN)?;
        message.edited = true;
        message.updated_at = Utc::now();
        state.messages.insert(message.id, message.clone());
        state.hub.publish(ChatBroadcast {
            event: "chat.message.updated".to_string(),
            channel_id: channel.id,
            message: message.clone(),
        });
        Ok(message)
    })();
    match result {
        Ok(message) => ok(message),
        Err(error) => err(error),
    }
}

async fn delete_message(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let actor = actor_id(&req)?;
        let (raw_channel, raw_message) = path.into_inner();
        let channel = load_channel(&state, &raw_channel)?;
        let message = load_message(&state, channel.id, &raw_message)?;
        if message.author_id != actor && channel.owner_id != actor {
            return Err(UnifiedError::forbidden(
                "Only the message author or the channel owner can delete the message",
            ));
        }
        state.messages.remove(&message.id);
        state.hub.publish(ChatBroadcast {
            event: "chat.message.deleted".to_string(),
            channel_id: channel.id,
            message: message.clone(),
        });
        Ok(json!({ "deleted": true, "message_id": message.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}

async fn toggle_reaction(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<ReactionRequest>,
) -> HttpResponse {
    let result: UnifiedResult<ChatMessage> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (raw_channel, raw_message) = path.into_inner();
        let channel = load_channel(&state, &raw_channel)?;
        if !can_view(&channel, &actor, &roles) {
            return Err(UnifiedError::forbidden("Channel is not visible to this user"));
        }
        let mut message = load_message(&state, channel.id, &raw_message)?;
        let emoji = require_text(&body.emoji, "emoji", 16)?;
        // Toggle: reacting with the same emoji again removes the reaction.
        if let Some(position) = message
            .reactions
            .iter()
            .position(|reaction| reaction.user_id == actor && reaction.emoji == emoji)
        {
            message.reactions.remove(position);
        } else {
            message.reactions.push(Reaction {
                user_id: actor,
                emoji,
            });
        }
        message.updated_at = Utc::now();
        state.messages.insert(message.id, message.clone());
        Ok(message)
    })();
    match result {
        Ok(message) => ok(message),
        Err(error) => err(error),
    }
}

async fn escalate_message(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<EscalateRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (raw_channel, raw_message) = path.into_inner();
        let channel = load_channel(&state, &raw_channel)?;
        if !can_view(&channel, &actor, &roles) {
            return Err(UnifiedError::forbidden("Channel is not visible to this user"));
        }
        let message = load_message(&state, channel.id, &raw_message)?;
        let title = match &body.title {
            Some(raw) => require_text(raw, "title", MAX_NAME_LEN)?,
            None => format!("Chat escalation from #{}", channel.name),
        };
        // ERP interconnection: the tickets module answers this hook by
        // creating a real ticket; chat only requests it. The payload must
        // carry the fields the tickets handler reads (`tickets.rs`,
        // `on_ticket_create_requested`): `title`, `body`, and `user_id`.
        // `description` is kept alongside `body` for older consumers.
        state.emit(
            "ticket.create_requested",
            json!({
                "source": "chat",
                "channel_id": channel.id.to_string(),
                "message_id": message.id.to_string(),
                "user_id": message.author_id.clone(),
                "author_id": message.author_id,
                "requested_by": actor,
                "title": title.clone(),
                "body": message.body.clone(),
                "description": message.body,
                "company_id": channel.company_id,
                "priority": "normal",
                "category": "chat",
            }),
        );
        Ok(json!({
            "ticket_requested": true,
            "channel_id": channel.id.to_string(),
            "message_id": message.id.to_string(),
            "title": title,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// File sharing handlers
// ---------------------------------------------------------------------------

async fn share_file(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ShareFileRequest>,
) -> HttpResponse {
    let result: UnifiedResult<ChatFile> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let channel = load_channel(&state, &path.into_inner())?;
        if !can_view(&channel, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "This user cannot share files in this channel",
            ));
        }
        let name = require_text(&body.name, "name", MAX_FILE_NAME_LEN)?;
        if body.size == 0 || body.size > MAX_FILE_SIZE {
            return Err(UnifiedError::bad_request(format!(
                "size must be between 1 and {MAX_FILE_SIZE} bytes"
            )));
        }
        let mime = require_text(&body.mime, "mime", MAX_MIME_LEN)?;
        // Optional storage reference: the bytes live in the storage
        // module, chat only records where to find them.
        let storage_target =
            clean_optional_text(&body.storage_target, "storage_target", MAX_STORAGE_REF_LEN)?;
        let storage_key =
            clean_optional_text(&body.storage_key, "storage_key", MAX_STORAGE_REF_LEN)?;
        let url = clean_optional_text(&body.url, "url", MAX_URL_LEN)?;
        if storage_key.is_some() && storage_target.is_none() {
            return Err(UnifiedError::bad_request(
                "storage_key requires a storage_target",
            ));
        }
        let file = ChatFile {
            id: Uuid::now_v7(),
            channel_id: channel.id,
            uploader_id: actor.clone(),
            name: name.clone(),
            size: body.size,
            mime: mime.clone(),
            storage_target: storage_target.clone(),
            storage_key: storage_key.clone(),
            url: url.clone(),
            created_at: Utc::now(),
        };
        state.files.insert(file.id, file.clone());
        state.emit(
            "file.shared",
            json!({
                "file_id": file.id.to_string(),
                "channel_id": channel.id.to_string(),
                "uploader_id": actor,
                "name": name,
                "size": body.size,
                "mime": mime,
                "storage_target": storage_target,
                "storage_key": storage_key,
                "url": url,
                "company_id": channel.company_id,
            }),
        );
        Ok(file)
    })();
    match result {
        Ok(file) => created(file),
        Err(error) => err(error),
    }
}

async fn list_files(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<ChatFile>> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let channel = load_channel(&state, &path.into_inner())?;
        if !can_view(&channel, &actor, &roles) {
            return Err(UnifiedError::forbidden("Channel is not visible to this user"));
        }
        let params = query.into_inner();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<ChatFile> = state
            .files
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|file| file.channel_id == channel.id)
            .filter(|file| {
                if search.is_empty() {
                    return true;
                }
                file.name.to_lowercase().contains(&search)
                    || file.mime.to_lowercase().contains(&search)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Direct message handlers
// ---------------------------------------------------------------------------

async fn create_dm(
    state: web::Data<ChatState>,
    req: HttpRequest,
    body: web::Json<CreateDmRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Channel> = (|| {
        let actor = actor_id(&req)?;
        let other = require_text(&body.user_id, "user_id", 128)?;
        if other == actor {
            return Err(UnifiedError::bad_request(
                "A direct message requires a different user",
            ));
        }
        let mut members = vec![actor.clone(), other.clone()];
        members.sort();
        // Find-or-create: a 1:1 channel is unique for its member pair.
        if let Some(existing) = state.channels.iter().find(|entry| {
            let channel = entry.value();
            channel.kind == ChannelKind::Dm && {
                let mut pair = channel.members.clone();
                pair.sort();
                pair == members
            }
        }) {
            return Ok(existing.value().clone());
        }
        let now = Utc::now();
        let channel = Channel {
            id: Uuid::now_v7(),
            name: format!("dm:{}:{}", members[0], members[1]),
            kind: ChannelKind::Dm,
            company_id: None,
            owner_id: actor.clone(),
            members,
            roles: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        state.channels.insert(channel.id, channel.clone());
        state.emit(
            "chat.channel.created",
            json!({
                "channel_id": channel.id.to_string(),
                "kind": "dm",
                "owner_id": actor,
                "members": channel.members.clone(),
            }),
        );
        Ok(channel)
    })();
    match result {
        Ok(channel) => created(channel),
        Err(error) => err(error),
    }
}

async fn list_dms(
    state: web::Data<ChatState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<Channel>> = (|| {
        let actor = actor_id(&req)?;
        let params = query.into_inner();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Channel> = state
            .channels
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|channel| {
                channel.kind == ChannelKind::Dm
                    && channel.members.iter().any(|member| member == &actor)
            })
            .filter(|channel| {
                if search.is_empty() {
                    return true;
                }
                channel
                    .members
                    .iter()
                    .any(|member| member.to_lowercase().contains(&search))
            })
            .collect();
        items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Presence handlers
// ---------------------------------------------------------------------------

async fn set_presence(
    state: web::Data<ChatState>,
    req: HttpRequest,
    body: web::Json<PresenceRequest>,
) -> HttpResponse {
    let result: UnifiedResult<PresenceEntry> = (|| {
        let actor = actor_id(&req)?;
        let entry = PresenceEntry {
            user_id: actor.clone(),
            status: body.status,
            last_seen: Utc::now(),
        };
        state.presence.insert(actor, entry.clone());
        Ok(entry)
    })();
    match result {
        Ok(entry) => ok(entry),
        Err(error) => err(error),
    }
}

async fn list_presence(
    state: web::Data<ChatState>,
    req: HttpRequest,
    query: web::Query<PresenceQuery>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<PresenceEntry>> = (|| {
        let _actor = actor_id(&req)?;
        let query = query.into_inner();
        let now = Utc::now();
        // Drop stale entries so presence never reports ghosts.
        state.presence.retain(|_, entry| {
            now.signed_duration_since(entry.last_seen).num_seconds() <= PRESENCE_STALE_SECS
        });
        let mut items: Vec<PresenceEntry> = state
            .presence
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| match &query.user_id {
                Some(user_id) => &entry.user_id == user_id,
                None => true,
            })
            .collect();
        items.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        let params = PaginationParams {
            page: query.page,
            per_page: query.per_page,
            sort: None,
            order: None,
            search: None,
            filters: std::collections::HashMap::new(),
        };
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// WebSocket handler
// ---------------------------------------------------------------------------

async fn chat_ws(
    state: web::Data<ChatState>,
    req: HttpRequest,
    stream: web::Payload,
    query: web::Query<WsQuery>,
) -> HttpResponse {
    let actor = match actor_id(&req) {
        Ok(actor) => actor,
        Err(error) => return err(error),
    };
    let channel_filter = match &query.channel {
        Some(raw) => match parse_uuid(raw, "channel") {
            Ok(id) => Some(id),
            Err(error) => return err(error),
        },
        None => None,
    };
    if let Some(channel_id) = channel_filter {
        match state.channels.get(&channel_id) {
            Some(entry) => {
                let roles = caller_roles(&req);
                if !can_view(entry.value(), &actor, &roles) {
                    return err(UnifiedError::forbidden(
                        "Channel is not visible to this user",
                    ));
                }
            }
            None => {
                return err(UnifiedError::not_found(format!(
                    "Channel not found: {channel_id}"
                )));
            }
        }
    }
    let (response, mut session, mut msg_stream) = match actix_ws::handle(&req, stream) {
        Ok(parts) => parts,
        Err(error) => {
            return err(
                UnifiedError::bad_request(format!("WebSocket handshake failed: {error}"))
                    .with_trace(format!("{error:?}")),
            );
        }
    };
    let mut receiver = state.hub.subscribe();
    actix_web::rt::spawn(async move {
        loop {
            tokio::select! {
                incoming = msg_stream.next() => {
                    match incoming {
                        Some(Ok(Message::Ping(bytes))) => {
                            if session.pong(&bytes).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                        Some(Ok(_)) => {}
                    }
                }
                broadcast = receiver.recv() => {
                    match broadcast {
                        Ok(event) => {
                            if let Some(channel_id) = channel_filter {
                                if event.channel_id != channel_id {
                                    continue;
                                }
                            }
                            let text = serde_json::to_string(&event).unwrap_or_default();
                            if session.text(text).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
        let _ = session.close(None).await;
    });
    response
}

// ---------------------------------------------------------------------------
// Live support: widget config + bootstrap
// ---------------------------------------------------------------------------

async fn get_widget_config(
    state: web::Data<ChatState>,
    query: web::Query<WidgetConfigQuery>,
) -> HttpResponse {
    let query = query.into_inner();
    let key = query.company_id.clone().unwrap_or_else(|| "default".to_string());
    let config = state
        .widget_configs
        .get(&key)
        .map(|entry| entry.value().clone())
        .unwrap_or_else(|| default_widget_config(query.company_id));
    ok(config)
}

async fn save_widget_config(
    state: web::Data<ChatState>,
    req: HttpRequest,
    body: web::Json<UpsertWidgetConfigRequest>,
) -> HttpResponse {
    let result: UnifiedResult<WidgetConfig> = (|| {
        let _actor = actor_id(&req)?;
        let body = body.into_inner();
        let company_id = clean_optional_text(&body.company_id, "company_id", MAX_DEPARTMENT_LEN)?;
        let key = company_id.clone().unwrap_or_else(|| "default".to_string());
        let now = Utc::now();
        let mut config = state
            .widget_configs
            .get(&key)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| default_widget_config(company_id.clone()));
        if let Some(name) = &body.name {
            config.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(greeting) = &body.greeting {
            config.greeting = require_text(greeting, "greeting", MAX_GREETING_LEN)?;
        }
        if let Some(offline_message) = &body.offline_message {
            config.offline_message =
                require_text(offline_message, "offline_message", MAX_GREETING_LEN)?;
        }
        if let Some(primary_color) = &body.primary_color {
            config.primary_color =
                require_text(primary_color, "primary_color", MAX_COLOR_LEN)?;
        }
        if let Some(departments) = body.departments {
            config.departments = clean_department_ids(Some(departments))?;
        }
        if let Some(enabled) = body.enabled {
            config.enabled = enabled;
        }
        config.company_id = company_id;
        config.updated_at = now;
        state.widget_configs.insert(key, config.clone());
        Ok(config)
    })();
    match result {
        Ok(config) => ok(config),
        Err(error) => err(error),
    }
}

/// Visitor bootstrap for the embeddable website widget. No account is
/// required: a visitor id is generated when the widget does not supply
/// one. Creates the backing private channel and the session, routes it
/// to an agent when one is free, and reports the queue position.
async fn bootstrap_widget_session(
    state: web::Data<ChatState>,
    body: web::Json<CreateSupportSessionRequest>,
) -> HttpResponse {
    let result: UnifiedResult<WidgetBootstrap> = (|| {
        let body = body.into_inner();
        let visitor_id = match &body.visitor_id {
            Some(raw) => require_text(raw, "visitor_id", MAX_DEPARTMENT_LEN)?,
            None => format!("visitor:{}", Uuid::now_v7()),
        };
        let visitor_name =
            clean_optional_text(&body.visitor_name, "visitor_name", MAX_VISITOR_NAME_LEN)?;
        let visitor_email = match &body.visitor_email {
            Some(raw) => Some(require_email(raw)?),
            None => None,
        };
        let department_id =
            clean_optional_text(&body.department_id, "department_id", MAX_DEPARTMENT_LEN)?;
        let subject = match &body.subject {
            Some(raw) => require_text(raw, "subject", MAX_SUBJECT_LEN)?,
            None => "Website chat".to_string(),
        };
        let company_id = clean_optional_text(&body.company_id, "company_id", MAX_DEPARTMENT_LEN)?;
        let session = create_support_session_inner(
            &state,
            visitor_id,
            visitor_name,
            visitor_email,
            department_id,
            subject,
            company_id,
        )?;
        Ok(WidgetBootstrap {
            channel_id: session.channel_id,
            queue_position: session.queue_position,
            agents_available: agents_available(&state, session.department_id.as_deref()),
            session,
        })
    })();
    match result {
        Ok(bootstrap) => created(bootstrap),
        Err(error) => err(error),
    }
}

/// Visitor polling: the widget re-reads its session (status, assigned
/// agent, queue position) with the unguessable session id. No account
/// is required.
async fn poll_widget_session(
    state: web::Data<ChatState>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<WidgetBootstrap> = (|| {
        refresh_queue_positions(&state);
        let session = load_session(&state, &path.into_inner())?;
        Ok(WidgetBootstrap {
            channel_id: session.channel_id,
            queue_position: session.queue_position,
            agents_available: agents_available(&state, session.department_id.as_deref()),
            session,
        })
    })();
    match result {
        Ok(bootstrap) => ok(bootstrap),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Live support: sessions
// ---------------------------------------------------------------------------

async fn list_support_sessions(
    state: web::Data<ChatState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<SupportSession>> = (|| {
        let _actor = actor_id(&req)?;
        refresh_queue_positions(&state);
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let department_filter = params.filters.get("department_id").cloned();
        let agent_filter = params.filters.get("agent_id").cloned();
        let visitor_filter = params.filters.get("visitor_id").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<SupportSession> = state
            .sessions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|session| {
                if let Some(status) = &status_filter {
                    let expected = format!("{:?}", session.status).to_lowercase();
                    if &expected != status {
                        return false;
                    }
                }
                if let Some(department) = &department_filter {
                    if session.department_id.as_deref() != Some(department.as_str()) {
                        return false;
                    }
                }
                if let Some(agent) = &agent_filter {
                    if session.agent_id.as_deref() != Some(agent.as_str()) {
                        return false;
                    }
                }
                if let Some(visitor) = &visitor_filter {
                    if &session.visitor_id != visitor {
                        return false;
                    }
                }
                if let Some(company) = &company_filter {
                    if session.company_id.as_deref() != Some(company.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !session.subject.to_lowercase().contains(&search)
                    && !session.visitor_id.to_lowercase().contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["created_at", "updated_at", "subject"], "created_at");
        // Newest first unless the caller asks for ascending order.
        let descending = params.normalized().order != "asc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "subject" => a.subject.cmp(&b.subject),
                "updated_at" => a.updated_at.cmp(&b.updated_at),
                _ => a.created_at.cmp(&b.created_at),
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

async fn create_support_session(
    state: web::Data<ChatState>,
    req: HttpRequest,
    body: web::Json<CreateSupportSessionRequest>,
) -> HttpResponse {
    let result: UnifiedResult<WidgetBootstrap> = (|| {
        let _actor = actor_id(&req)?;
        let body = body.into_inner();
        let visitor_id = match &body.visitor_id {
            Some(raw) => require_text(raw, "visitor_id", MAX_DEPARTMENT_LEN)?,
            None => {
                return Err(UnifiedError::bad_request(
                    "visitor_id is required for staff-created sessions",
                ))
            }
        };
        let visitor_name =
            clean_optional_text(&body.visitor_name, "visitor_name", MAX_VISITOR_NAME_LEN)?;
        let visitor_email = match &body.visitor_email {
            Some(raw) => Some(require_email(raw)?),
            None => None,
        };
        let department_id =
            clean_optional_text(&body.department_id, "department_id", MAX_DEPARTMENT_LEN)?;
        let subject = match &body.subject {
            Some(raw) => require_text(raw, "subject", MAX_SUBJECT_LEN)?,
            None => "Website chat".to_string(),
        };
        let company_id = clean_optional_text(&body.company_id, "company_id", MAX_DEPARTMENT_LEN)?;
        let session = create_support_session_inner(
            &state,
            visitor_id,
            visitor_name,
            visitor_email,
            department_id,
            subject,
            company_id,
        )?;
        Ok(WidgetBootstrap {
            channel_id: session.channel_id,
            queue_position: session.queue_position,
            agents_available: agents_available(&state, session.department_id.as_deref()),
            session,
        })
    })();
    match result {
        Ok(bootstrap) => created(bootstrap),
        Err(error) => err(error),
    }
}

async fn get_support_session(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<SupportSession> = (|| {
        let _actor = actor_id(&req)?;
        refresh_queue_positions(&state);
        load_session(&state, &path.into_inner())
    })();
    match result {
        Ok(session) => ok(session),
        Err(error) => err(error),
    }
}

async fn accept_support_session(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<SupportSession> = (|| {
        let actor = actor_id(&req)?;
        if !state.agents.contains_key(&actor) {
            return Err(UnifiedError::not_found(format!(
                "Agent not registered: {actor}"
            )));
        }
        let mut session = load_session(&state, &path.into_inner())?;
        if session.status != SupportSessionStatus::Waiting {
            return Err(UnifiedError::conflict(format!(
                "Only a waiting session can be accepted (status is {:?})",
                session.status
            )));
        }
        if let Some(department) = &session.department_id {
            let serves = state
                .agents
                .get(&actor)
                .map(|entry| entry.department_ids.iter().any(|id| id == department))
                .unwrap_or(false);
            if !serves {
                return Err(UnifiedError::forbidden(
                    "This agent does not serve the session department",
                ));
            }
        }
        session.agent_id = Some(actor.clone());
        session.status = SupportSessionStatus::Active;
        session.updated_at = Utc::now();
        add_agent_to_channel(&state, &session);
        state.sessions.insert(session.id, session.clone());
        refresh_queue_positions(&state);
        state.emit(
            "chat.session.accepted",
            json!({
                "session_id": session.id.to_string(),
                "agent_id": actor.clone(),
                "visitor_id": session.visitor_id.clone(),
                "department_id": session.department_id.clone(),
            }),
        );
        state.emit(
            "chat.session.assigned",
            json!({
                "session_id": session.id.to_string(),
                "agent_id": actor,
                "department_id": session.department_id.clone(),
                "visitor_id": session.visitor_id.clone(),
            }),
        );
        Ok(session)
    })();
    match result {
        Ok(session) => ok(session),
        Err(error) => err(error),
    }
}

async fn close_support_session(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CloseSessionRequest>,
) -> HttpResponse {
    let result: UnifiedResult<SupportSession> = (|| {
        let actor = actor_id(&req)?;
        if !state.agents.contains_key(&actor) {
            return Err(UnifiedError::not_found(format!(
                "Agent not registered: {actor}"
            )));
        }
        let mut session = load_session(&state, &path.into_inner())?;
        if session.status != SupportSessionStatus::Active
            && session.status != SupportSessionStatus::Waiting
        {
            return Err(UnifiedError::conflict(format!(
                "Only an active or waiting session can be closed (status is {:?})",
                session.status
            )));
        }
        // A waiting session that never reached an agent is missed, not
        // closed: it tells reporting the visitor was never served.
        if session.status == SupportSessionStatus::Waiting {
            session.status = SupportSessionStatus::Missed;
        } else {
            session.status = SupportSessionStatus::Closed;
        }
        session.close_reason = match &body.reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_NOTE_LEN)?),
            None => None,
        };
        session.closed_at = Some(Utc::now());
        session.updated_at = Utc::now();
        state.sessions.insert(session.id, session.clone());
        refresh_queue_positions(&state);
        state.emit(
            "chat.session.closed",
            json!({
                "session_id": session.id.to_string(),
                "agent_id": session.agent_id.clone(),
                "visitor_id": session.visitor_id.clone(),
                "department_id": session.department_id.clone(),
                "status": format!("{:?}", session.status).to_lowercase(),
                "closed_by": actor,
            }),
        );
        Ok(session)
    })();
    match result {
        Ok(session) => ok(session),
        Err(error) => err(error),
    }
}

async fn transfer_support_session(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TransferSessionRequest>,
) -> HttpResponse {
    let result: UnifiedResult<SupportSession> = (|| {
        let actor = actor_id(&req)?;
        if !state.agents.contains_key(&actor) {
            return Err(UnifiedError::not_found(format!(
                "Agent not registered: {actor}"
            )));
        }
        let to_agent_id = require_text(&body.to_agent_id, "to_agent_id", MAX_DEPARTMENT_LEN)?;
        if !state.agents.contains_key(&to_agent_id) {
            return Err(UnifiedError::not_found(format!(
                "Target agent not registered: {to_agent_id}"
            )));
        }
        let note = match &body.note {
            Some(raw) => require_text(raw, "note", MAX_NOTE_LEN)?,
            None => String::new(),
        };
        let mut session = load_session(&state, &path.into_inner())?;
        if session.status != SupportSessionStatus::Active
            && session.status != SupportSessionStatus::Waiting
        {
            return Err(UnifiedError::conflict(format!(
                "Only an active or waiting session can be transferred (status is {:?})",
                session.status
            )));
        }
        if session.agent_id.as_deref() == Some(to_agent_id.as_str()) {
            return Err(UnifiedError::conflict(
                "The session is already assigned to this agent",
            ));
        }
        let from_agent_id = session.agent_id.clone();
        session.transfer_history.push(TransferRecord {
            from_agent_id: from_agent_id.clone(),
            to_agent_id: to_agent_id.clone(),
            note: note.clone(),
            transferred_by: actor.clone(),
            created_at: Utc::now(),
        });
        session.agent_id = Some(to_agent_id.clone());
        session.status = SupportSessionStatus::Active;
        session.updated_at = Utc::now();
        add_agent_to_channel(&state, &session);
        state.sessions.insert(session.id, session.clone());
        refresh_queue_positions(&state);
        let handover = if note.is_empty() {
            format!(
                "Chat transferred from {} to {}.",
                from_agent_id.as_deref().unwrap_or("unassigned"),
                to_agent_id
            )
        } else {
            format!(
                "Chat transferred from {} to {}: {note}",
                from_agent_id.as_deref().unwrap_or("unassigned"),
                to_agent_id
            )
        };
        post_system_message(&state, session.channel_id, handover);
        state.emit(
            "chat.session.transferred",
            json!({
                "session_id": session.id.to_string(),
                "from_agent_id": from_agent_id,
                "to_agent_id": to_agent_id,
                "transferred_by": actor,
                "note": note,
                "visitor_id": session.visitor_id.clone(),
                "department_id": session.department_id.clone(),
            }),
        );
        Ok(session)
    })();
    match result {
        Ok(session) => ok(session),
        Err(error) => err(error),
    }
}

async fn escalate_support_session(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<EscalateSessionRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let actor = actor_id(&req)?;
        let mut session = load_session(&state, &path.into_inner())?;
        if session.escalated_ticket_id.is_some() {
            return Err(UnifiedError::conflict(
                "This session was already escalated to a ticket",
            ));
        }
        let title = match &body.title {
            Some(raw) => require_text(raw, "title", MAX_NAME_LEN)?,
            None => format!("Chat transcript: {}", session.subject),
        };
        let priority = match &body.priority {
            Some(raw) => {
                let cleaned = require_text(raw, "priority", 32)?;
                if !valid_ticket_priority(&cleaned) {
                    return Err(UnifiedError::bad_request(
                        "priority must be one of: low, normal, high, urgent",
                    ));
                }
                cleaned
            }
            None => "normal".to_string(),
        };
        let category = match &body.category {
            Some(raw) => require_text(raw, "category", MAX_NAME_LEN)?,
            None => "chat".to_string(),
        };
        // The full transcript becomes the ticket body so the agent who
        // picks the ticket up sees the whole conversation.
        let transcript = build_transcript_text(&state, &session);
        let ticket_id = request_ticket(
            &state,
            json!({
                "source": "chat",
                "session_id": session.id.to_string(),
                "channel_id": session.channel_id.to_string(),
                "user_id": session.visitor_id.clone(),
                "requested_by": actor.clone(),
                "title": title.clone(),
                "body": transcript.clone(),
                "description": format!(
                    "Chat session {} escalated by {}",
                    session.id, actor
                ),
                "company_id": session.company_id.clone(),
                "department_id": session.department_id.clone(),
                "priority": priority,
                "category": category,
            }),
        )?;
        session.escalated_ticket_id = ticket_id.clone();
        session.updated_at = Utc::now();
        state.sessions.insert(session.id, session.clone());
        state.emit(
            "chat.session.escalated",
            json!({
                "session_id": session.id.to_string(),
                "ticket_id": ticket_id,
                "escalated_by": actor,
                "visitor_id": session.visitor_id.clone(),
                "department_id": session.department_id.clone(),
            }),
        );
        Ok(json!({
            "ticket_requested": true,
            "ticket_id": ticket_id,
            "session_id": session.id.to_string(),
            "title": title,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}

async fn get_session_transcript(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<TranscriptQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _actor = actor_id(&req)?;
        let session = load_session(&state, &path.into_inner())?;
        let mut messages: Vec<ChatMessage> = state
            .messages
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|message| message.channel_id == session.channel_id)
            .collect();
        messages.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
        let mut files: Vec<ChatFile> = state
            .files
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|file| file.channel_id == session.channel_id)
            .collect();
        files.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let rating = state
            .ratings
            .iter()
            .map(|entry| entry.value().clone())
            .find(|rating| rating.session_id == session.id);
        let format = query
            .into_inner()
            .format
            .unwrap_or_else(|| "json".to_string());
        if format == "text" {
            return Ok(json!({
                "session_id": session.id.to_string(),
                "format": "text",
                "transcript": build_transcript_text(&state, &session),
            }));
        }
        let transcript = SessionTranscript {
            session,
            messages,
            files,
            rating,
        };
        serde_json::to_value(transcript)
            .map_err(|_| UnifiedError::bad_request("Transcript could not be serialized"))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}

async fn get_queue_position(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _actor = actor_id(&req)?;
        refresh_queue_positions(&state);
        let session = load_session(&state, &path.into_inner())?;
        let waiting_ahead = if session.status == SupportSessionStatus::Waiting {
            session.queue_position.saturating_sub(1)
        } else {
            0
        };
        Ok(json!({
            "session_id": session.id.to_string(),
            "status": format!("{:?}", session.status).to_lowercase(),
            "position": session.queue_position,
            "waiting_ahead": waiting_ahead,
            "agents_available": agents_available(&state, session.department_id.as_deref()),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Live support: ratings
// ---------------------------------------------------------------------------

async fn rate_support_session(
    state: web::Data<ChatState>,
    path: web::Path<String>,
    body: web::Json<RateSessionRequest>,
) -> HttpResponse {
    let result: UnifiedResult<ChatRating> = (|| {
        let session = load_session(&state, &path.into_inner())?;
        if session.status != SupportSessionStatus::Closed {
            return Err(UnifiedError::conflict(
                "Only a closed chat can be rated",
            ));
        }
        if state
            .ratings
            .iter()
            .any(|entry| entry.value().session_id == session.id)
        {
            return Err(UnifiedError::conflict(
                "This chat has already been rated",
            ));
        }
        if body.score == 0 || body.score > MAX_RATING_SCORE {
            return Err(UnifiedError::bad_request(format!(
                "score must be between 1 and {MAX_RATING_SCORE}"
            )));
        }
        let comment = match &body.comment {
            Some(raw) => Some(require_text(raw, "comment", MAX_RATING_COMMENT_LEN)?),
            None => None,
        };
        let visitor_id = match &body.visitor_id {
            Some(raw) => {
                let cleaned = require_text(raw, "visitor_id", MAX_DEPARTMENT_LEN)?;
                if cleaned != session.visitor_id {
                    return Err(UnifiedError::forbidden(
                        "Only the chat visitor can rate this chat",
                    ));
                }
                cleaned
            }
            None => session.visitor_id.clone(),
        };
        let rating = ChatRating {
            id: Uuid::now_v7(),
            session_id: session.id,
            visitor_id: visitor_id.clone(),
            agent_id: session.agent_id.clone(),
            department_id: session.department_id.clone(),
            score: body.score,
            comment,
            created_at: Utc::now(),
        };
        state.ratings.insert(rating.id, rating.clone());
        state.emit(
            "chat.session.rated",
            json!({
                "session_id": session.id.to_string(),
                "rating_id": rating.id.to_string(),
                "visitor_id": visitor_id,
                "agent_id": rating.agent_id.clone(),
                "department_id": rating.department_id.clone(),
                "score": rating.score,
            }),
        );
        Ok(rating)
    })();
    match result {
        Ok(rating) => created(rating),
        Err(error) => err(error),
    }
}

async fn get_session_rating(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ChatRating> = (|| {
        let _actor = actor_id(&req)?;
        let session = load_session(&state, &path.into_inner())?;
        state
            .ratings
            .iter()
            .map(|entry| entry.value().clone())
            .find(|rating| rating.session_id == session.id)
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "No rating for support session: {}",
                    session.id
                ))
            })
    })();
    match result {
        Ok(rating) => ok(rating),
        Err(error) => err(error),
    }
}

async fn list_ratings(
    state: web::Data<ChatState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<ChatRating>> = (|| {
        let _actor = actor_id(&req)?;
        let params = query.into_inner();
        let agent_filter = params.filters.get("agent_id").cloned();
        let department_filter = params.filters.get("department_id").cloned();
        let score_filter = params.filters.get("score").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<ChatRating> = state
            .ratings
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rating| {
                if let Some(agent) = &agent_filter {
                    if rating.agent_id.as_deref() != Some(agent.as_str()) {
                        return false;
                    }
                }
                if let Some(department) = &department_filter {
                    if rating.department_id.as_deref() != Some(department.as_str()) {
                        return false;
                    }
                }
                if let Some(score) = &score_filter {
                    if rating.score.to_string() != *score {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let comment = rating.comment.clone().unwrap_or_default().to_lowercase();
                    if !comment.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        // Newest first unless the caller asks for ascending order.
        let descending = params.normalized().order != "asc";
        items.sort_by(|a, b| {
            if descending {
                b.created_at.cmp(&a.created_at)
            } else {
                a.created_at.cmp(&b.created_at)
            }
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Live support: queue
// ---------------------------------------------------------------------------

async fn list_queue(
    state: web::Data<ChatState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<SupportSession>> = (|| {
        let _actor = actor_id(&req)?;
        refresh_queue_positions(&state);
        let params = query.into_inner();
        let department_filter = params.filters.get("department_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<SupportSession> = state
            .sessions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|session| session.status == SupportSessionStatus::Waiting)
            .filter(|session| {
                if let Some(department) = &department_filter {
                    if session.department_id.as_deref() != Some(department.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !session.subject.to_lowercase().contains(&search)
                    && !session.visitor_id.to_lowercase().contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        // Queue order: oldest waiting visitor first.
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Live support: agents
// ---------------------------------------------------------------------------

async fn list_agents(
    state: web::Data<ChatState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<SupportAgent>> = (|| {
        let _actor = actor_id(&req)?;
        let params = query.into_inner();
        let department_filter = params.filters.get("department_id").cloned();
        let available_filter = params.filters.get("available").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<SupportAgent> = state
            .agents
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|agent| {
                if let Some(department) = &department_filter {
                    if !agent.department_ids.iter().any(|id| id == department) {
                        return false;
                    }
                }
                if let Some(available) = &available_filter {
                    let expected = available == "true";
                    let effective = agent_availability_rank(&state, agent).is_some();
                    if effective != expected {
                        return false;
                    }
                }
                if !search.is_empty() && !agent.user_id.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

/// An agent registers (or updates) their own routing record. The caller
/// is the agent: `X-User-Id` is the roster key.
async fn register_agent(
    state: web::Data<ChatState>,
    req: HttpRequest,
    body: web::Json<UpsertAgentRequest>,
) -> HttpResponse {
    let result: UnifiedResult<SupportAgent> = (|| {
        let actor = actor_id(&req)?;
        let body = body.into_inner();
        let now = Utc::now();
        let mut agent = state
            .agents
            .get(&actor)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| SupportAgent {
                user_id: actor.clone(),
                department_ids: Vec::new(),
                available: true,
                max_concurrent: DEFAULT_MAX_CONCURRENT_CHATS,
                created_at: now,
                updated_at: now,
            });
        if let Some(departments) = body.department_ids {
            agent.department_ids = clean_department_ids(Some(departments))?;
        }
        if let Some(available) = body.available {
            agent.available = available;
        }
        if let Some(max_concurrent) = body.max_concurrent {
            if max_concurrent == 0 {
                return Err(UnifiedError::bad_request(
                    "max_concurrent must be at least 1",
                ));
            }
            agent.max_concurrent = max_concurrent;
        }
        agent.updated_at = now;
        state.agents.insert(actor, agent.clone());
        Ok(agent)
    })();
    match result {
        Ok(agent) => ok(agent),
        Err(error) => err(error),
    }
}

fn load_agent(state: &ChatState, user_id: &str) -> UnifiedResult<SupportAgent> {
    state
        .agents
        .get(user_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Agent not registered: {user_id}")))
}

async fn get_agent(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<SupportAgent> = (|| {
        let _actor = actor_id(&req)?;
        load_agent(&state, &path.into_inner())
    })();
    match result {
        Ok(agent) => ok(agent),
        Err(error) => err(error),
    }
}

async fn update_agent(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateAgentRequest>,
) -> HttpResponse {
    let result: UnifiedResult<SupportAgent> = (|| {
        let actor = actor_id(&req)?;
        let user_id = path.into_inner();
        if actor != user_id {
            return Err(UnifiedError::forbidden(
                "Only the agent themselves can update their routing record",
            ));
        }
        let body = body.into_inner();
        let mut agent = load_agent(&state, &user_id)?;
        if let Some(departments) = body.department_ids {
            agent.department_ids = clean_department_ids(Some(departments))?;
        }
        if let Some(available) = body.available {
            agent.available = available;
        }
        if let Some(max_concurrent) = body.max_concurrent {
            if max_concurrent == 0 {
                return Err(UnifiedError::bad_request(
                    "max_concurrent must be at least 1",
                ));
            }
            agent.max_concurrent = max_concurrent;
        }
        agent.updated_at = Utc::now();
        state.agents.insert(user_id, agent.clone());
        Ok(agent)
    })();
    match result {
        Ok(agent) => ok(agent),
        Err(error) => err(error),
    }
}

async fn remove_agent(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let actor = actor_id(&req)?;
        let user_id = path.into_inner();
        if actor != user_id {
            return Err(UnifiedError::forbidden(
                "Only the agent themselves can remove their routing record",
            ));
        }
        let _agent = load_agent(&state, &user_id)?;
        state.agents.remove(&user_id);
        Ok(json!({ "deleted": true, "user_id": user_id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Live support: canned responses
// ---------------------------------------------------------------------------

async fn list_canned(
    state: web::Data<ChatState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<CannedResponse>> = (|| {
        let _actor = actor_id(&req)?;
        let params = query.into_inner();
        let category_filter = params.filters.get("category").cloned();
        let department_filter = params.filters.get("department_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<CannedResponse> = state
            .canned
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|canned| {
                if let Some(category) = &category_filter {
                    if &canned.category != category {
                        return false;
                    }
                }
                if let Some(department) = &department_filter {
                    if canned.department_id.as_deref() != Some(department.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !canned.title.to_lowercase().contains(&search)
                    && !canned.body.to_lowercase().contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["title", "category", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a.title.cmp(&b.title),
                "category" => a.category.cmp(&b.category),
                _ => a.created_at.cmp(&b.created_at),
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

async fn create_canned(
    state: web::Data<ChatState>,
    req: HttpRequest,
    body: web::Json<CreateCannedRequest>,
) -> HttpResponse {
    let result: UnifiedResult<CannedResponse> = (|| {
        let actor = actor_id(&req)?;
        let body = body.into_inner();
        let now = Utc::now();
        let canned = CannedResponse {
            id: Uuid::now_v7(),
            title: require_text(&body.title, "title", MAX_CANNED_TITLE_LEN)?,
            body: require_text(&body.body, "body", MAX_CANNED_BODY_LEN)?,
            category: match &body.category {
                Some(raw) => require_text(raw, "category", MAX_NAME_LEN)?,
                None => "general".to_string(),
            },
            department_id: clean_optional_text(
                &body.department_id,
                "department_id",
                MAX_DEPARTMENT_LEN,
            )?,
            created_by: actor.clone(),
            usage_count: 0,
            created_at: now,
            updated_at: now,
        };
        state.canned.insert(canned.id, canned.clone());
        state.emit(
            "chat.canned_response.created",
            json!({
                "canned_response_id": canned.id.to_string(),
                "title": canned.title.clone(),
                "category": canned.category.clone(),
                "department_id": canned.department_id.clone(),
                "created_by": actor,
            }),
        );
        Ok(canned)
    })();
    match result {
        Ok(canned) => created(canned),
        Err(error) => err(error),
    }
}

fn load_canned(state: &ChatState, raw_id: &str) -> UnifiedResult<CannedResponse> {
    let id = parse_uuid(raw_id, "canned response")?;
    state
        .canned
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Canned response not found: {id}")))
}

async fn get_canned(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<CannedResponse> = (|| {
        let _actor = actor_id(&req)?;
        load_canned(&state, &path.into_inner())
    })();
    match result {
        Ok(canned) => ok(canned),
        Err(error) => err(error),
    }
}

async fn update_canned(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCannedRequest>,
) -> HttpResponse {
    let result: UnifiedResult<CannedResponse> = (|| {
        let actor = actor_id(&req)?;
        let body = body.into_inner();
        let mut canned = load_canned(&state, &path.into_inner())?;
        if canned.created_by != actor {
            return Err(UnifiedError::forbidden(
                "Only the canned response author can update it",
            ));
        }
        if let Some(title) = &body.title {
            canned.title = require_text(title, "title", MAX_CANNED_TITLE_LEN)?;
        }
        if let Some(body_text) = &body.body {
            canned.body = require_text(body_text, "body", MAX_CANNED_BODY_LEN)?;
        }
        if let Some(category) = &body.category {
            canned.category = require_text(category, "category", MAX_NAME_LEN)?;
        }
        if let Some(department_id) = &body.department_id {
            canned.department_id = match department_id {
                Some(raw) => Some(require_text(raw, "department_id", MAX_DEPARTMENT_LEN)?),
                None => None,
            };
        }
        canned.updated_at = Utc::now();
        state.canned.insert(canned.id, canned.clone());
        state.emit(
            "chat.canned_response.updated",
            json!({
                "canned_response_id": canned.id.to_string(),
                "updated_by": actor,
            }),
        );
        Ok(canned)
    })();
    match result {
        Ok(canned) => ok(canned),
        Err(error) => err(error),
    }
}

async fn delete_canned(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let actor = actor_id(&req)?;
        let canned = load_canned(&state, &path.into_inner())?;
        if canned.created_by != actor {
            return Err(UnifiedError::forbidden(
                "Only the canned response author can delete it",
            ));
        }
        state.canned.remove(&canned.id);
        state.emit(
            "chat.canned_response.deleted",
            json!({
                "canned_response_id": canned.id.to_string(),
                "deleted_by": actor,
            }),
        );
        Ok(json!({ "deleted": true, "canned_response_id": canned.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}

/// Use a canned response: counts the use (so teams can see which replies
/// actually help) and returns the body for the agent to send.
async fn use_canned(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UseCannedRequest>,
) -> HttpResponse {
    let result: UnifiedResult<CannedResponse> = (|| {
        let actor = actor_id(&req)?;
        let mut canned = load_canned(&state, &path.into_inner())?;
        canned.usage_count = canned.usage_count.saturating_add(1);
        canned.updated_at = Utc::now();
        state.canned.insert(canned.id, canned.clone());
        state.emit(
            "chat.canned_response.used",
            json!({
                "canned_response_id": canned.id.to_string(),
                "used_by": actor,
                "session_id": body.session_id.clone(),
            }),
        );
        Ok(canned)
    })();
    match result {
        Ok(canned) => ok(canned),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Live support: offline messages
// ---------------------------------------------------------------------------

async fn list_offline_messages(
    state: web::Data<ChatState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<OfflineMessage>> = (|| {
        let _actor = actor_id(&req)?;
        let params = query.into_inner();
        let department_filter = params.filters.get("department_id").cloned();
        let ticket_filter = params.filters.get("ticket_requested").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<OfflineMessage> = state
            .offline_messages
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|message| {
                if let Some(department) = &department_filter {
                    if message.department_id.as_deref() != Some(department.as_str()) {
                        return false;
                    }
                }
                if let Some(ticket_requested) = &ticket_filter {
                    let expected = ticket_requested == "true";
                    if message.ticket_requested != expected {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !message.subject.to_lowercase().contains(&search)
                    && !message.message.to_lowercase().contains(&search)
                    && !message.visitor_email.to_lowercase().contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        // Newest first unless the caller asks for ascending order.
        let descending = params.normalized().order != "asc";
        items.sort_by(|a, b| {
            if descending {
                b.created_at.cmp(&a.created_at)
            } else {
                a.created_at.cmp(&b.created_at)
            }
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

/// A visitor leaves a message when no agent is available. When agents
/// are free the widget should start a live chat instead, so this
/// endpoint refuses with a conflict in that case. Otherwise the message
/// becomes a ticket through `ticket.create_requested` and is kept here
/// for staff follow-up.
async fn create_offline_message(
    state: web::Data<ChatState>,
    body: web::Json<CreateOfflineMessageRequest>,
) -> HttpResponse {
    let result: UnifiedResult<OfflineMessage> = (|| {
        let body = body.into_inner();
        let visitor_name = require_text(&body.visitor_name, "visitor_name", MAX_VISITOR_NAME_LEN)?;
        let visitor_email = require_email(&body.visitor_email)?;
        let department_id =
            clean_optional_text(&body.department_id, "department_id", MAX_DEPARTMENT_LEN)?;
        let subject = match &body.subject {
            Some(raw) => require_text(raw, "subject", MAX_SUBJECT_LEN)?,
            None => format!("Offline message from {visitor_name}"),
        };
        let message_text = require_text(&body.message, "message", MAX_OFFLINE_MESSAGE_LEN)?;
        if agents_available(&state, department_id.as_deref()) {
            return Err(UnifiedError::conflict(
                "Live agents are available for this department; start a live chat instead",
            ));
        }
        let ticket_id = request_ticket(
            &state,
            json!({
                "source": "chat",
                "channel_id": Value::Null,
                "user_id": visitor_email.clone(),
                "visitor_name": visitor_name.clone(),
                "visitor_email": visitor_email.clone(),
                "title": subject.clone(),
                "body": message_text.clone(),
                "description": message_text.clone(),
                "company_id": Value::Null,
                "department_id": department_id.clone(),
                "priority": "normal",
                "category": "chat",
            }),
        )?;
        let offline = OfflineMessage {
            id: Uuid::now_v7(),
            visitor_name: visitor_name.clone(),
            visitor_email: visitor_email.clone(),
            department_id: department_id.clone(),
            subject: subject.clone(),
            message: message_text,
            ticket_requested: ticket_id.is_some(),
            ticket_id: ticket_id.clone(),
            created_at: Utc::now(),
        };
        state.offline_messages.insert(offline.id, offline.clone());
        state.emit(
            "chat.offline_message.created",
            json!({
                "offline_message_id": offline.id.to_string(),
                "visitor_email": visitor_email,
                "department_id": department_id,
                "ticket_id": ticket_id,
            }),
        );
        Ok(offline)
    })();
    match result {
        Ok(offline) => created(offline),
        Err(error) => err(error),
    }
}
