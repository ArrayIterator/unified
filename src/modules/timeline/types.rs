//! Activity Timeline module. - Types.
//!
//! Split from `src/modules/timeline.rs`; behaviour unchanged.

use super::handlers::*;
use super::helpers::*;
use super::module::*;
use super::state::*;

use chrono::DateTime;
use chrono::Utc;
use serde::Serialize;
use serde_json::Value;

/// Maximum entries kept in the timeline cache. The oldest entries are
/// evicted first; the durable audit / records stores are unaffected.
pub const MAX_ENTRIES: usize = 5_000;

#[derive(Debug, Clone, Serialize)]
pub struct TimelineEntry {
    pub id: String,
    pub seq: u64,
    pub actor: String,
    /// The hook event name that produced this entry.
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub summary: String,
    /// Which source family produced the entry: `record`, `audit` or
    /// `task`. Failure events report `security`, `cluster` or
    /// `payment`.
    pub source: String,
    /// Whitelisted summary of the event only (`summary`,
    /// `entity_type`, `entity_id`, `actor`). The raw event payload
    /// is never stored here: it can carry secrets, tokens,
    /// passwords, user ids and IPs, and the feed serves `details`.
    pub details: Value,
    /// Never populated: client IPs from event payloads are not
    /// retained in the timeline cache. The field stays so the
    /// serialized entry shape is unchanged; the durable audit log
    /// remains the source for IPs.
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CountEntry {
    pub key: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimelineCounts {
    pub total: u64,
    pub by_action: Vec<CountEntry>,
    pub by_entity_type: Vec<CountEntry>,
    pub by_day: Vec<CountEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DayGroup {
    pub date: String,
    pub count: u64,
}
