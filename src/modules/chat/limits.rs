//! chat - limits.

use super::{types::*, hub::*, state::*, helpers::*, module::*, handlers_channels::*, handlers_messages::*, handlers_files::*, handlers_direct::*, handlers_presence::*, handlers_websocket::*, support_config::*, support_sessions::*, support_ratings::*, support_agents::*, support_canned::*, support_offline::*};


/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 100;

pub(crate) const MAX_BODY_LEN: usize = 10_000;

pub(crate) const MAX_MEMBERS: usize = 500;

pub(crate) const MAX_FILE_SIZE: u64 = 512 * 1024 * 1024;

pub(crate) const MAX_FILE_NAME_LEN: usize = 255;

pub(crate) const MAX_MIME_LEN: usize = 128;

/// Presence entries older than this are stale and hidden.
pub(crate) const PRESENCE_STALE_SECS: i64 = 60;

/// Broadcast channel capacity for the WebSocket hub.
pub(crate) const HUB_CAPACITY: usize = 256;

/// Live support limits.
pub(crate) const MAX_SUBJECT_LEN: usize = 200;

pub(crate) const MAX_VISITOR_NAME_LEN: usize = 100;

pub(crate) const MAX_EMAIL_LEN: usize = 254;

pub(crate) const MAX_DEPARTMENT_LEN: usize = 128;

pub(crate) const MAX_GREETING_LEN: usize = 500;

pub(crate) const MAX_COLOR_LEN: usize = 16;

pub(crate) const MAX_CANNED_TITLE_LEN: usize = 100;

pub(crate) const MAX_CANNED_BODY_LEN: usize = 5_000;

pub(crate) const MAX_NOTE_LEN: usize = 2_000;

pub(crate) const MAX_RATING_COMMENT_LEN: usize = 1_000;

pub(crate) const MAX_OFFLINE_MESSAGE_LEN: usize = 5_000;

pub(crate) const MAX_STORAGE_REF_LEN: usize = 512;

pub(crate) const MAX_URL_LEN: usize = 2_048;

pub(crate) const DEFAULT_MAX_CONCURRENT_CHATS: u32 = 5;

pub(crate) const MAX_RATING_SCORE: u8 = 5;
