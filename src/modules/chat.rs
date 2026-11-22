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
//   - `ticket.create_requested` (on message escalation)

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::HookBus;
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry};
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatFile {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub uploader_id: String,
    pub name: String,
    pub size: u64,
    pub mime: String,
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
        }
    }
}

impl ChatState {
    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.hooks.read().expect("hooks lock poisoned").clone() {
            bus.emit(event, data);
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
        "Real-time messaging: channels, direct messages, reactions, file sharing, presence, and a WebSocket hub."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "company", "notifications"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
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
                .route("/ws", web::get().to(chat_ws)),
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
        // creating a real ticket; chat only requests it.
        state.emit(
            "ticket.create_requested",
            json!({
                "source": "chat",
                "channel_id": channel.id.to_string(),
                "message_id": message.id.to_string(),
                "author_id": message.author_id,
                "requested_by": actor,
                "title": title.clone(),
                "description": message.body,
                "company_id": channel.company_id,
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
        let file = ChatFile {
            id: Uuid::now_v7(),
            channel_id: channel.id,
            uploader_id: actor.clone(),
            name: name.clone(),
            size: body.size,
            mime: mime.clone(),
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
