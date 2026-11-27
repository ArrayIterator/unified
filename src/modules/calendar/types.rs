//! calendar module - Items, feeds, settings, sources and request shapes.

use unified_core::pagination::PaginationParams;
use chrono::{DateTime, FixedOffset, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use uuid::Uuid;

use super::constants::DEFAULT_ITEM_COLOR;
use super::constants::DEFAULT_VIEW;
use super::constants::DEFAULT_WEEKEND_DAYS;
use super::constants::DEFAULT_WORK_END;
use super::constants::DEFAULT_WORK_START;

// Shared item shape.

/// One dated item contributed by an owning module. Unknown JSON
/// fields are ignored so modules may attach extras without
/// breaking aggregation; items missing a required field are
/// skipped (counted in the response `skipped` field) instead of
/// failing the whole view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarItem {
    pub id: String,
    pub module: String,
    pub title: String,
    pub start: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<DateTime<Utc>>,
    #[serde(default)]
    pub all_day: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deep_link: Option<String>,
    #[serde(default = "default_visibility")]
    pub visibility: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    /// Tenant the item belongs to, when the contributing module
    /// knows it. Items carrying a tenant_id different from the
    /// caller's tenant are excluded server-side (see
    /// `aggregate_items`); an item with no tenant_id is trusted
    /// only when `is_system` is set - otherwise it is excluded
    /// the same way, so an unscoped item from another tenant's
    /// module can never leak in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    /// Explicit system/global marker. Only a contributing module
    /// that deliberately marks an item system/global may leave its
    /// tenant_id unset; the marker defaults to false, so ordinary
    /// tenant-less items are untrusted (see `aggregate_items`).
    #[serde(default)]
    pub is_system: bool,
    /// Start rendered in the viewer's timezone (view responses
    /// only; never supplied by contributing modules).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_local: Option<String>,
    /// End rendered in the viewer's timezone (view responses only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_local: Option<String>,
}

pub(crate) fn default_visibility() -> String {
    "shared".to_string()
}

impl CalendarItem {
    /// Effective end for overlap math: the stated end, else the
    /// start plus one hour (an all-day item with no end spans its
    /// start day: start plus 24 hours).
    pub(crate) fn effective_end(&self) -> DateTime<Utc> {
        if let Some(end) = self.end {
            return end;
        }
        if self.all_day {
            self.start + chrono::Duration::hours(24)
        } else {
            self.start + chrono::Duration::hours(1)
        }
    }

    /// Render the UTC bounds in the viewer's fixed offset.
    pub(crate) fn with_local_times(&self, offset: FixedOffset) -> CalendarItem {
        let mut item = self.clone();
        item.start_local = Some(self.start.with_timezone(&offset).to_rfc3339());
        item.end_local = Some(self.effective_end().with_timezone(&offset).to_rfc3339());
        if item.color.is_none() {
            item.color = Some(DEFAULT_ITEM_COLOR.to_string());
        }
        item
    }

    /// Public JSON view of one item (local times already applied).
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "module": self.module,
            "title": self.title,
            "start": self.start.to_rfc3339(),
            "end": self.end.map(|value| value.to_rfc3339()),
            "all_day": self.all_day,
            "color": self.color.clone().unwrap_or_else(|| DEFAULT_ITEM_COLOR.to_string()),
            "label": self.label,
            "deep_link": self.deep_link,
            "visibility": self.visibility,
            "owner_user_id": self.owner_user_id,
            "company_id": self.company_id,
            "tenant_id": self.tenant_id,
            "is_system": self.is_system,
            "start_local": self.start_local,
            "end_local": self.end_local,
        })
    }
}

/// One non-working day inside a view range.
#[derive(Debug, Clone, Serialize)]
pub struct NonWorkingDay {
    pub date: String,
    /// "weekend" or "holiday".
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// One overlap hint: two items of the same owner whose intervals
/// intersect. Hints only - the calendar never blocks a creation.
#[derive(Debug, Clone, Serialize)]
pub struct ConflictHint {
    pub first_module: String,
    pub first_id: String,
    pub second_module: String,
    pub second_id: String,
    pub overlap_start: String,
    pub overlap_end: String,
}

// Stored records (in-memory authoritative store; the migration
// defines the persistence targets).

/// A revocable iCal subscription feed. Only the SHA-256 hash of
/// the issued token is stored - never the raw token.
#[derive(Debug, Clone, Serialize)]
pub struct CalendarFeed {
    pub id: Uuid,
    /// Tenant the feed belongs to. A feed token presented under a
    /// different tenant answers 404, never leaks.
    pub tenant_id: String,
    /// Company the feed owner belongs to (from the tenant context
    /// at creation), so the feed renders what the owner could see.
    pub company_id: Option<String>,
    pub user_id: String,
    #[serde(skip_serializing)]
    pub token_hash: String,
    /// Module scope: empty means every module the caller may see.
    pub modules: Vec<String>,
    pub label: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub revoked: bool,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

/// Effective settings, merged from company defaults and the
/// caller's own overrides (user values win when set).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarSettings {
    #[serde(default)]
    pub weekend_days: Vec<u8>,
    #[serde(default = "default_work_start")]
    pub working_hours_start: String,
    #[serde(default = "default_work_end")]
    pub working_hours_end: String,
    #[serde(default = "default_view_string")]
    pub default_view: String,
    /// Stored timezone: "UTC" or a fixed offset ("+07:00").
    /// IANA names are not supported (no chrono-tz in Cargo.toml).
    #[serde(default = "default_timezone_string")]
    pub timezone: String,
    /// ISO 3166-1 alpha-2 holiday country, or empty when no
    /// country is configured (then the holiday hook is skipped).
    #[serde(default)]
    pub holiday_country: String,
    /// Deep-link templates for create-intent fallback, keyed by
    /// module id. Empty means "use the built-in defaults".
    #[serde(default)]
    pub create_link_templates: HashMap<String, String>,
}

pub(crate) fn default_work_start() -> String {
    DEFAULT_WORK_START.to_string()
}

pub(crate) fn default_work_end() -> String {
    DEFAULT_WORK_END.to_string()
}

pub(crate) fn default_view_string() -> String {
    DEFAULT_VIEW.to_string()
}

pub(crate) fn default_timezone_string() -> String {
    "UTC".to_string()
}

impl Default for CalendarSettings {
    fn default() -> Self {
        Self {
            weekend_days: DEFAULT_WEEKEND_DAYS.to_vec(),
            working_hours_start: DEFAULT_WORK_START.to_string(),
            working_hours_end: DEFAULT_WORK_END.to_string(),
            default_view: DEFAULT_VIEW.to_string(),
            timezone: "UTC".to_string(),
            holiday_country: String::new(),
            create_link_templates: HashMap::new(),
        }
    }
}

/// One per-user, per-module visibility toggle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarSource {
    /// Tenant the toggle belongs to (part of the lookup key).
    pub tenant_id: String,
    pub user_id: String,
    pub module: String,
    pub visible: bool,
    pub updated_at: DateTime<Utc>,
}

// Request / response shapes.

#[derive(Debug, Deserialize)]
pub(crate) struct CalendarViewQuery {
    pub(crate) view: Option<String>,
    pub(crate) from: Option<String>,
    pub(crate) to: Option<String>,
    pub(crate) modules: Option<String>,
    pub(crate) timezone: Option<String>,
    /// Optional standard pagination (`?page=` / `?per_page=`).
    /// A date-range view normally returns every item in the range
    /// (the calendar UI needs them all to render); pagination only
    /// applies when the caller explicitly asks for a page.
    #[serde(flatten)]
    pub(crate) pagination: PaginationParams,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CalendarItemsQuery {
    pub(crate) from: Option<String>,
    pub(crate) to: Option<String>,
    pub(crate) modules: Option<String>,
    pub(crate) timezone: Option<String>,
    #[serde(flatten)]
    pub(crate) pagination: PaginationParams,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateIntentRequest {
    pub(crate) module: String,
    pub(crate) start: String,
    pub(crate) end: Option<String>,
    pub(crate) title: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateSettingsRequest {
    pub(crate) scope: Option<String>,
    pub(crate) weekend_days: Option<Vec<u8>>,
    pub(crate) working_hours_start: Option<String>,
    pub(crate) working_hours_end: Option<String>,
    pub(crate) default_view: Option<String>,
    pub(crate) timezone: Option<String>,
    pub(crate) holiday_country: Option<String>,
    pub(crate) create_link_templates: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateSourcesRequest {
    pub(crate) sources: Vec<SourceToggleInput>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SourceToggleInput {
    pub(crate) module: String,
    pub(crate) visible: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateFeedRequest {
    pub(crate) modules: Option<Vec<String>>,
    pub(crate) expires_in_days: Option<i64>,
    pub(crate) label: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AiDraftRequest {
    pub(crate) date: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AiDraftResponse {
    pub(crate) kind: String,
    pub(crate) draft: String,
    pub(crate) ai_generated: bool,
    pub(crate) status: String,
    pub(crate) item_ids: Vec<String>,
    pub(crate) user_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) provider: Option<String>,
    pub(crate) requested_at: DateTime<Utc>,
}
