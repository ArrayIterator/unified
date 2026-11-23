// AI Support module.
//
// Split into submodules; the full module documentation lives in `module.rs`.

pub mod ai_capabilities;
pub mod types;
pub mod state;
mod helpers;
pub mod module;
mod handlers_ask;
mod handlers_conversations;
mod handlers_handoff;
mod handlers_connectors;

pub(crate) use self::types::{MAX_MESSAGE_LEN, MAX_SUMMARY_LEN, MAX_TITLE_LEN,
    MAX_CONNECTOR_NAME_LEN, GROUNDING_LIMIT};
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::{ConversationStatus, MessageRole, ArticleSource, Conversation,
    ConversationMessage, ConversationDetail, ConnectorKind, Connector, AskRequest, HandoffRequest,
    CreateTicketRequest, UpdateConversationRequest, CreateConnectorRequest, UpdateConnectorRequest};
pub use self::state::{AiSupportState};
pub(crate) use self::helpers::{
    Caller, PERM_CONNECTORS_MANAGE, actor_id, authorize_connectors, caller, caller_company,
    change_conversation_status, connector_visible_to, created, err, grounded_answer,
    load_conversation, no_articles_answer, ok, paginate, parse_sources, parse_uuid, push_message,
    redact_connector, require_caller_company, require_connector_access, require_tenant,
    require_text,
};
pub use self::module::{AiSupportModule, register};
pub(crate) use self::handlers_ask::{ask};
pub(crate) use self::handlers_conversations::{list_conversations, get_conversation,
    update_conversation, delete_conversation};
pub(crate) use self::handlers_handoff::{do_handoff, handoff, create_ticket};
pub(crate) use self::handlers_connectors::{create_connector, list_connectors, get_connector,
    update_connector, delete_connector};
