// Webinars module: scheduled and recurring live webinars with
// registrations, attendance, polls, Q&A, chat, recordings and
// analytics. Split into one file per concern; this file only declares
// the submodules and re-exports the public API.

pub mod ai_capabilities;
pub mod entitlement;
pub mod handlers_analytics;
pub mod handlers_attendance;
pub mod handlers_chat;
pub mod handlers_config;
pub mod handlers_followup;
pub mod handlers_polls;
pub mod handlers_questions;
pub mod handlers_recordings;
pub mod handlers_registrations;
pub mod handlers_webinars;
pub mod helpers;
pub mod module;
pub mod scheduler;
pub mod state;
pub mod types;

// Re-exported from types.rs.
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::AnalyticsQuery;
pub use self::types::AskQuestionRequest;
pub use self::types::AttachRecordingRequest;
pub use self::types::AttendanceView;
pub use self::types::CreatePollRequest;
pub use self::types::CreateWebinarRequest;
pub use self::types::DropOffBucket;
pub use self::types::FollowUpRequest;
pub use self::types::LinkMeetingRequest;
pub use self::types::LiveView;
pub use self::types::MuteRequest;
pub use self::types::PollOptionResult;
pub use self::types::PollView;
pub use self::types::PostChatRequest;
pub use self::types::QuestionView;
pub use self::types::ReplayView;
pub use self::types::UpdateConfigRequest;
pub use self::types::UpdateWebinarRequest;
pub use self::types::VotePollRequest;
pub use self::types::Webinar;
pub use self::types::WebinarAccessModel;
pub use self::types::WebinarAnalytics;
pub use self::types::WebinarAttendance;
pub use self::types::WebinarConfig;
pub use self::types::WebinarMuteState;
pub use self::types::WebinarPoll;
pub use self::types::WebinarPollVote;
pub use self::types::WebinarQuestion;
pub use self::types::WebinarRecording;
pub use self::types::WebinarRegistration;
pub use self::types::WebinarRole;
pub use self::types::WebinarStatus;
// Re-exported from state.rs.
pub use self::state::WebinarsState;
// Re-exported from module.rs.
pub use self::module::WebinarsModule;
pub use self::module::register;
