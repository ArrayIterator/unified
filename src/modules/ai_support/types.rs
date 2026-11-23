// AI support types and request bodies.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_MESSAGE_LEN: usize = 10_000;
pub(crate) const MAX_SUMMARY_LEN: usize = 5_000;
pub(crate) const MAX_TITLE_LEN: usize = 200;
pub(crate) const MAX_CONNECTOR_NAME_LEN: usize = 100;
/// How many knowledgebase articles ground one answer.
pub(crate) const GROUNDING_LIMIT: u64 = 5;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Conversation lifecycle.
///
/// Allowed status transitions (the machine is enforced on every
/// write; anything else is rejected with 409 Conflict):
///
///   - `open` -> `handoff` (human handoff), `open` -> `closed`
///   - `handoff` -> `closed`
///   - `closed` -> `open` (explicit reopen only, via PATCH on the
///     conversation; asking a question never reopens a closed
///     conversation)
///   - staying in the same status is a no-op and always allowed
///
/// In particular `closed` -> `handoff` and `handoff` -> `open` are
/// rejected: a closed conversation must be reopened first, and a
/// handed-off conversation is closed, never silently returned to
/// the assistant queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConversationStatus {
    Open,
    Handoff,
    Closed,
}

impl ConversationStatus {
    /// Lowercase wire name (`"open"`, `"handoff"`, `"closed"`).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Handoff => "handoff",
            Self::Closed => "closed",
        }
    }

    /// Statuses `from` may move to (excluding the same-status no-op,
    /// which is always allowed).
    pub(crate) fn allowed_transitions(from: Self) -> &'static [Self] {
        match from {
            Self::Open => &[Self::Handoff, Self::Closed],
            Self::Handoff => &[Self::Closed],
            Self::Closed => &[Self::Open],
        }
    }

    /// True when a move from `from` to `to` is allowed by the
    /// lifecycle table above (same-status moves are allowed).
    pub(crate) fn can_transition(from: Self, to: Self) -> bool {
        from == to || Self::allowed_transitions(from).contains(&to)
    }
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
///
/// LIMITATION (documented, not implemented): enabling a connector
/// only stores its configuration. No live connection to the
/// external chat service is established — bot token validation,
/// webhook registration, and inbound message delivery are a
/// follow-up milestone, not a silent no-op: nothing here claims a
/// connector is connected. Until that milestone lands, connectors
/// are configuration records only; inbound chat still arrives
/// through the built-in webchat channel.
///
/// Connectors are owned per user, exactly like conversations: the
/// creator is the owner (`owner_id`) and only the owner — or a
/// platform admin (`admin` / `super_admin` role) — may read, update
/// or delete one. The stored `config` is kept verbatim internally
/// but is never returned to callers as-is: every response carries a
/// redacted copy with secret-bearing fields masked (see
/// `redact_connector` in `helpers.rs`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connector {
    pub id: Uuid,
    pub name: String,
    pub kind: ConnectorKind,
    pub enabled: bool,
    pub config: Value,
    /// Owner of this connector: the `X-User-Id` of its creator.
    pub owner_id: String,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transition_table_allows_only_the_documented_moves() {
        use ConversationStatus::*;
        // Allowed moves (plus the same-status no-op).
        assert!(ConversationStatus::can_transition(Open, Open));
        assert!(ConversationStatus::can_transition(Open, Handoff));
        assert!(ConversationStatus::can_transition(Open, Closed));
        assert!(ConversationStatus::can_transition(Handoff, Handoff));
        assert!(ConversationStatus::can_transition(Handoff, Closed));
        assert!(ConversationStatus::can_transition(Closed, Closed));
        assert!(ConversationStatus::can_transition(Closed, Open));
        // Rejected moves.
        assert!(!ConversationStatus::can_transition(Handoff, Open));
        assert!(!ConversationStatus::can_transition(Closed, Handoff));
    }

    #[test]
    fn allowed_transitions_lists_each_source_exactly() {
        use ConversationStatus::*;
        assert_eq!(
            ConversationStatus::allowed_transitions(Open),
            &[Handoff, Closed]
        );
        assert_eq!(
            ConversationStatus::allowed_transitions(Handoff),
            &[Closed]
        );
        assert_eq!(
            ConversationStatus::allowed_transitions(Closed),
            &[Open]
        );
    }
}
