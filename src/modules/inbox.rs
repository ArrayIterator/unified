//! Inbox module - Smart Inbox module - one queue unifying email, chat, tickets, and
//!
//! Split into submodules by concern; the public API is
//! re-exported below. The full route map and module notes
//! live in `inbox/module.rs`.

pub mod ai_capabilities;
pub mod types;
mod state;
mod auth;
mod validation;
mod service;
mod events;
mod handlers_threads;
mod handlers_replies;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::{AssignRequest, CreateCannedReplyRequest, MergeRequest, RegisterThreadRequest,
    ReplyRequest, SnoozeRequest, UpdateCannedReplyRequest, UpdateThreadRequest};
pub(crate) use self::types::{CHANNELS, CHAT_MESSAGE, CHAT_SESSION_ASSIGNED, CHAT_SESSION_CREATED,
    InboxAssignment, InboxCannedReply, InboxThread, MAIL_SENT, MAX_BODY_LEN, MAX_ID_LEN,
    MAX_PARTICIPANTS, MAX_SUBJECT_LEN, MAX_TITLE_LEN, PERM_INBOX_MANAGE, PERM_INBOX_VIEW,
    PRIORITIES, SCHEDULER_KIND_UNSNOOZE, STATUSES, TICKET_ASSIGNED, TICKET_COMMENTED,
    TICKET_CREATED, TICKET_UPDATED, UNSNOOZE_REQUESTED};
pub(crate) use self::state::{InboxState};
pub(crate) use self::auth::{Caller, caller, check_capability, created, err, ok, optional_text,
    paginate, parse_uuid, require_permission, require_text};
pub(crate) use self::validation::{priority_rank, ticket_status_to_inbox, valid_channel,
    valid_priority, valid_status};
pub(crate) use self::service::{assignments_for, find_thread_for_source, load_canned_reply,
    load_thread, opt_datetime, opt_string, schedule_unsnooze, thread_view, upsert_thread};
pub(crate) use self::events::{on_chat_message, on_chat_session, on_mail_sent, on_ticket_event,
    on_unsnooze_requested};
pub(crate) use self::handlers_threads::{assign_thread, get_thread, list_threads, mark_read,
    mark_unread, merge_thread, queue_stats, register_thread, request_reply, snooze_thread,
    unmerge_thread, unsnooze_thread, update_thread};
pub(crate) use self::handlers_replies::{create_canned_reply, delete_canned_reply, get_canned_reply,
    list_canned_replies, update_canned_reply};
pub use self::module::{InboxModule, register};
