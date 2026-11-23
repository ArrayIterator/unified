//! chat - types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use super::{limits::*, hub::*, state::*, helpers::*, module::*, handlers_channels::*, handlers_messages::*, handlers_files::*, handlers_direct::*, handlers_presence::*, handlers_websocket::*, support_config::*, support_sessions::*, support_ratings::*, support_agents::*, support_canned::*, support_offline::*};



#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------
/// Channel visibility.
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



#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
// ---------------------------------------------------------------------------
// Live support types
// ---------------------------------------------------------------------------
/// Lifecycle of a live support session.
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



#[derive(Debug, Deserialize)]
// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------
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
