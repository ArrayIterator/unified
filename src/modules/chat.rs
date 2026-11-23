// Chat module.

// Split into one file per concern under `src/modules/chat/`;
// this root only declares the submodules and re-exports the
// public API so every item stays reachable as `crate::modules::chat::Item`.

pub mod ai_capabilities;
mod limits;
pub mod types;
pub mod hub;
pub mod state;
mod helpers;
pub mod module;
mod handlers_channels;
mod handlers_messages;
mod handlers_files;
mod handlers_direct;
mod handlers_presence;
mod handlers_websocket;
mod support_config;
mod support_sessions;
mod support_ratings;
mod support_agents;
mod support_canned;
mod support_offline;

pub(crate) use self::limits::{DEFAULT_MAX_CONCURRENT_CHATS, HUB_CAPACITY, MAX_BODY_LEN, MAX_CANNED_BODY_LEN, MAX_CANNED_TITLE_LEN, MAX_COLOR_LEN, MAX_DEPARTMENT_LEN, MAX_EMAIL_LEN, MAX_FILE_NAME_LEN, MAX_FILE_SIZE, MAX_GREETING_LEN, MAX_MEMBERS, MAX_MIME_LEN, MAX_NAME_LEN, MAX_NOTE_LEN, MAX_OFFLINE_MESSAGE_LEN, MAX_RATING_COMMENT_LEN, MAX_RATING_SCORE, MAX_STORAGE_REF_LEN, MAX_SUBJECT_LEN, MAX_URL_LEN, MAX_VISITOR_NAME_LEN, PRESENCE_STALE_SECS};
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::{AddMemberRequest, CannedResponse, Channel, ChannelKind, ChatBroadcast, ChatFile, ChatMessage, ChatRating, CloseSessionRequest, CreateCannedRequest, CreateChannelRequest, CreateDmRequest, CreateOfflineMessageRequest, CreateSupportSessionRequest, EscalateRequest, EscalateSessionRequest, OfflineMessage, PostMessageRequest, PresenceEntry, PresenceQuery, PresenceRequest, PresenceStatus, RateSessionRequest, Reaction, ReactionRequest, SessionTranscript, ShareFileRequest, SupportAgent, SupportSession, SupportSessionStatus, TranscriptQuery, TransferRecord, TransferSessionRequest, UpdateAgentRequest, UpdateCannedRequest, UpdateChannelRequest, UpdateMessageRequest, UpsertAgentRequest, UpsertWidgetConfigRequest, UseCannedRequest, WidgetBootstrap, WidgetConfig, WidgetConfigQuery, WsQuery};
pub use self::hub::{ChatHub};
pub use self::state::{ChatState};
pub(crate) use self::helpers::{active_chat_count, actor_id, add_agent_to_channel, agent_availability_rank, agents_available, best_available_agent, build_transcript_text, caller_roles, can_view, clean_department_ids, clean_members, clean_optional_text, clean_roles, create_support_session_inner, created, default_widget_config, department_agent_ids, err, find_mentions, load_channel, load_message, load_session, ok, paginate, parse_uuid, post_system_message, presence_rank, refresh_queue_positions, request_ticket, require_email, require_text, route_session, valid_ticket_priority};
pub use self::module::{ChatModule, register};
pub(crate) use self::handlers_channels::{add_member, create_channel, delete_channel, get_channel, list_channels, remove_member, update_channel};
pub(crate) use self::handlers_messages::{delete_message, escalate_message, list_messages, post_message, toggle_reaction, update_message};
pub(crate) use self::handlers_files::{list_files, share_file};
pub(crate) use self::handlers_direct::{create_dm, list_dms};
pub(crate) use self::handlers_presence::{list_presence, set_presence};
pub(crate) use self::handlers_websocket::{chat_ws};
pub(crate) use self::support_config::{bootstrap_widget_session, get_widget_config, list_queue, poll_widget_session, save_widget_config};
pub(crate) use self::support_sessions::{accept_support_session, close_support_session, create_support_session, escalate_support_session, get_queue_position, get_session_transcript, get_support_session, list_support_sessions, transfer_support_session};
pub(crate) use self::support_ratings::{get_session_rating, list_ratings, rate_support_session};
pub(crate) use self::support_agents::{get_agent, list_agents, load_agent, register_agent, remove_agent, update_agent};
pub(crate) use self::support_canned::{create_canned, delete_canned, get_canned, list_canned, load_canned, update_canned, use_canned};
pub(crate) use self::support_offline::{create_offline_message, list_offline_messages};
