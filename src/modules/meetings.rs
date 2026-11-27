//! Meetings module - Zoom-like video meetings over WebRTC.
//!
//! Split into one file per concern under `meetings/`; this file only
//! declares the submodules and re-exports the public API. The full
//! module design notes live at the top of `meetings/module.rs`.
pub mod ai_capabilities;
pub mod types;
mod helpers;
mod validation;
mod hooks;
mod handlers_rooms;
mod handlers_participants;
mod handlers_messages;
mod handlers_recordings;
mod handlers_history;
mod websocket;
pub mod module;

// Re-exports from types.rs.
pub(crate) use self::types::MAX_TITLE_LEN;
pub(crate) use self::types::MAX_DESCRIPTION_LEN;
pub(crate) use self::types::MAX_TIMEZONE_LEN;
pub(crate) use self::types::MAX_RECURRENCE_LEN;
pub(crate) use self::types::MAX_CHAT_BODY_LEN;
pub(crate) use self::types::MAX_PARTICIPANTS;
pub(crate) use self::types::MIN_PARTICIPANTS;
pub(crate) use self::types::DEFAULT_MAX_PARTICIPANTS;
pub(crate) use self::types::PRESENCE_STALE_SECS;
pub(crate) use self::types::RECORDING_SIGNED_URL_SECS;
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::MeetingRole;
pub use self::types::RoomStatus;
pub use self::types::AccessModel;
pub use self::types::MeetingRoom;
pub use self::types::RoomWithToken;
pub use self::types::Participant;
pub use self::types::MeetingMessage;
pub use self::types::Recording;
pub use self::types::AttendanceView;
pub use self::types::PresenceView;
pub use self::types::JoinResult;
pub use self::types::CreateRoomRequest;
pub use self::types::UpdateRoomRequest;
pub use self::types::PresenceHeartbeatRequest;
pub use self::types::ScreenShareRequest;
pub use self::types::PostMeetingMessageRequest;
pub use self::types::SignalRequest;
pub use self::types::SignalKind;
pub use self::types::AttachRecordingRequest;
pub use self::types::HistoryQuery;
pub use self::types::JoinRoomQuery;
pub use self::types::MeetingsWsQuery;
pub use self::types::MeetingsState;

// Re-exports from helpers.rs.
pub(crate) use self::helpers::actor_id;
pub(crate) use self::helpers::caller_role;
pub(crate) use self::helpers::can_manage_room;
pub(crate) use self::helpers::created;
pub(crate) use self::helpers::err;
pub(crate) use self::helpers::has_host_override;
pub(crate) use self::helpers::is_co_host;
pub(crate) use self::helpers::is_host;
pub(crate) use self::helpers::joined_count;
pub(crate) use self::helpers::load_participant;
pub(crate) use self::helpers::load_room;
pub(crate) use self::helpers::ok;
pub(crate) use self::helpers::paginate;
pub(crate) use self::helpers::parse_uuid;
pub(crate) use self::helpers::participant_key;
pub(crate) use self::helpers::require_tenant;
pub(crate) use self::helpers::require_text;
pub(crate) use self::helpers::role_name;

// Re-exports from validation.rs.
pub(crate) use self::validation::validate_calendar;
pub(crate) use self::validation::validate_calendar_with;
pub(crate) use self::validation::check_entitlement;
pub(crate) use self::validation::schedule_meeting;

// Re-exports from hooks.rs.
pub(crate) use self::hooks::on_meeting_start_requested;
pub(crate) use self::hooks::relay_signal;

// Re-exports from handlers_rooms.rs.
pub(crate) use self::handlers_rooms::list_rooms;
pub(crate) use self::handlers_rooms::create_room;
pub(crate) use self::handlers_rooms::get_room;
pub(crate) use self::handlers_rooms::update_room;
pub(crate) use self::handlers_rooms::delete_room;
pub(crate) use self::handlers_rooms::start_room;
pub(crate) use self::handlers_rooms::end_room;
pub(crate) use self::handlers_rooms::cancel_room;
pub(crate) use self::handlers_rooms::regenerate_token;
pub(crate) use self::handlers_rooms::join_by_token;

// Re-exports from handlers_participants.rs.
pub(crate) use self::handlers_participants::join_room;
pub(crate) use self::handlers_participants::leave_room;
pub(crate) use self::handlers_participants::list_participants;
pub(crate) use self::handlers_participants::promote_participant;
pub(crate) use self::handlers_participants::demote_participant;
pub(crate) use self::handlers_participants::remove_participant;
pub(crate) use self::handlers_participants::admit_waiting;
pub(crate) use self::handlers_participants::deny_waiting;
pub(crate) use self::handlers_participants::list_presence;
pub(crate) use self::handlers_participants::heartbeat_presence;
pub(crate) use self::handlers_participants::set_screen_share;

// Re-exports from handlers_messages.rs.
pub(crate) use self::handlers_messages::list_messages;
pub(crate) use self::handlers_messages::post_message;
pub(crate) use self::handlers_messages::post_signal;

// Re-exports from handlers_recordings.rs.
pub(crate) use self::handlers_recordings::list_recordings;
pub(crate) use self::handlers_recordings::start_recording;
pub(crate) use self::handlers_recordings::stop_recording;
pub(crate) use self::handlers_recordings::attach_recording;

// Re-exports from handlers_history.rs.
pub(crate) use self::handlers_history::list_attendance;
pub(crate) use self::handlers_history::list_history;

// Re-exports from websocket.rs.
pub(crate) use self::websocket::meetings_ws;

// Re-exports from module.rs.
pub use self::module::MeetingsModule;
pub use self::module::register;

