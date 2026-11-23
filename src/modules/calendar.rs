// calendar module.
//
// Split into focused submodules under src/modules/calendar/; the full
// module documentation kept in calendar/constants.rs.

mod aggregation;
mod ai_handlers;
pub mod constants;
mod handlers;
mod helpers;
mod hooks;
mod ics;
pub mod module;
pub mod state;
pub mod types;
mod validation;

pub use self::constants::{CALENDAR_AI_CAPABILITIES, DEFAULT_CREATE_LINK_TEMPLATES};
pub use self::constants::{CALENDAR_VIEWS, PERM_AI_USE, PERM_CALENDAR_MANAGE, PERM_CALENDAR_READ};
pub use self::constants::{DEFAULT_FEED_TTL_DAYS, MAX_FEED_TTL_DAYS, MIN_FEED_TTL_DAYS};
pub use self::constants::{DEFAULT_ITEM_COLOR, DEFAULT_WEEKEND_DAYS, DEFAULT_WORK_END};
pub use self::constants::{DEFAULT_VIEW, MAX_ITEMS_RANGE_DAYS, MAX_MODULE_FILTER, MAX_VIEW_DAYS};
pub use self::constants::{DEFAULT_WORK_START, MAX_FEEDS_PER_USER, MAX_SOURCES_PER_USER};
pub use self::constants::{EVENT_AI_DRAFT_REQUESTED, EVENT_FEEDS_PRUNE_REQUESTED};
pub use self::constants::{EVENT_AI_SUMMARIZE_DAY_REQUESTED, EVENT_AI_WHATS_NEXT_REQUESTED};
pub use self::constants::{EVENT_CREATE_INTENT_REQUESTED, EVENT_VIEW_REQUESTED};
pub use self::constants::{EVENT_CREATE_REQUESTED, EVENT_ITEMS_REQUESTED};
pub use self::constants::{EVENT_HOLIDAY_LOOKUP, SCHEDULER_JOB_KIND_FEEDS_PRUNE};
pub use self::constants::{ITEM_VISIBILITIES, MAX_FEED_LABEL_LEN, MAX_MODULE_ID_LEN};
pub use self::constants::{MAX_AI_ITEMS, MAX_HOLIDAY_LOOKUP_DAYS, MAX_ITEMS_PER_VIEW};
pub use self::module::{CalendarModule, register};
pub use self::state::CalendarState;
pub use self::types::CalendarSource;
pub use self::types::{CalendarFeed, CalendarItem, CalendarSettings, ConflictHint, NonWorkingDay};
