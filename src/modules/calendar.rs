// Calendar module — the integrated aggregation view.
//
// This module OWNS NO dated records. It does not store events,
// appointments, meetings, or todos, and it never creates, moves, or
// deletes a dated item. It aggregates dated items from every other
// module through one hook contract (`calendar.items_requested`) and
// renders them as month / week / day / agenda views, a paginated
// item list, per-user iCalendar subscription feeds, and AI draft
// summaries. Creating anything from the calendar is a hand-off:
// `POST /api/v1/calendar/create-intent` returns the owning module's
// create deep link (or the URL its `calendar.create_requested`
// handler answers with) and the calendar itself writes nothing.
//
// Shared item shape (the contract every answering module returns):
//   {
//     "id": "550e8400-...",            // item id inside its module
//     "module": "appointments",       // owning module id
//     "title": "Dentist check-up",
//     "start": "2026-11-23T02:00:00Z", // RFC 3339, UTC
//     "end": "2026-11-23T03:00:00Z",   // optional; defaults to start + 1h for overlap hints
//     "all_day": false,
//     "color": "blue",                 // optional display hint
//     "label": "Appointment",          // optional display label
//     "deep_link": "/appointments/550e8400-...",
//     "visibility": "private",         // "private" | "shared" | "public"
//     "owner_user_id": "user-1",       // optional
//     "company_id": "company-1"        // optional; empty means unscoped
//   }
//
// Hook contracts this module defines (see the matching `pub const`
// names below for the exact event strings):
//   - `calendar.items_requested` — request:
//       {"tenant_id", "company_id", "user_id", "start", "end",
//        "modules": [...]}
//     (start/end are RFC 3339 UTC bounds; "modules" is an optional
//     filter list; omit it to ask every module). Each answering
//     module contributes ONE result: either a JSON array of items in
//     the shape above, or an object {"items": [...]}. Results are
//     merged, sorted by start, de-duplicated by (module, id).
//     Visibility is enforced HERE, server-side, after merging:
//     private items reach only their owner, and items whose
//     company_id differs from the caller's company are excluded
//     silently (never a 403-vs-404 signal). Items carrying a
//     tenant_id different from the caller's tenant are excluded
//     the same way. When no module answers,
//     the calendar still works and returns an empty item list.
//   - `calendar.create_requested` — request:
//       {"tenant_id", "module", "start", "end", "title", "user_id",
//        "company_id"}.
//     Answer: {"url": "<owning module create link with the date
//     pre-filled>"}. When no module answers, a configurable
//     deep-link template (see `DEFAULT_CREATE_LINK_TEMPLATES`,
//     admin-editable through settings) is used; when neither exists
//     the endpoint answers 503 instead of inventing a link.
//   - `calendar.ai.draft_requested` — request:
//       {"user_id", "role", "kind", "date", "context_text",
//        "item_ids", "requested_at"}. Answer: {"draft": "..."} (or
//        {"text": "..."}), optionally with "model"/"provider". The
//        AI never creates, moves, or deletes calendar items; drafts
//        only. When no AI handler answers, the endpoint answers 503
//        instead of fabricating a summary.
//   - `calendar.feeds.prune_requested` — the scheduler-facing
//     listener name for the `calendar.feeds.prune` job kind. The
//     listener revokes/expired-prunes feed tokens. NOTE: the
//     scheduler's `KNOWN_KINDS` does not contain
//     `calendar.feeds.prune` yet, so today the scheduler would
//     decline the job; the listener and the pruning logic are ready
//     and the follow-up is to add the kind (see the OPEN list in the
//     module report).
//   - `holidays.is_holiday` (CONSUMED, owned by the holidays
//     module, which may not exist yet) — request:
//       {"date": "2026-11-23", "country": "ID",
//        "tenant_id": "<uuidv7>"} where "country" is an ISO 3166-1
//       alpha-2 code and "tenant_id" is REQUIRED (the holidays
//       handler answers nothing without it, fail closed). Expected
//       answer: one result object {"is_holiday": true,
//       "name": "Christmas Day", "source": "synced",
//       "provider": "nager-date"} — "source" is "synced" or
//       "manual", the provider id travels in "provider". When no
//       handler is registered the hook simply returns no results
//       and non-working days fall back to the configured weekend
//       days (see below); the calendar never errors because the
//       holidays module is absent.
//
// Tenant scoping (standing rule — read before changing anything
// below). Calendar feeds, settings, and source toggles are TENANT
// business data. Every record carries `tenant_id`, every lookup
// key is prefixed with the tenant id, and every aggregated item
// is filtered by tenant. The tenant context is resolved ONLY from
// `crate::core::tenancy::TenantContext::from_http_request` (the
// batch-17 tenancy context: parsed uuidv7 tenant id plus the
// `schema_name` derived ONLY by `tenancy::schema_name()` from it)
// — never from a client-supplied header, and never from a
// hard-coded tenant id, schema name, or package id. NO TENANT
// CONTEXT MEANS FAIL CLOSED: every handler (including the public
// iCal feed, whose token must belong to the resolved tenant)
// answers 401/403 when the request did not resolve to a tenant;
// nothing silently falls back to the `public` schema. OPEN:
// control-plane requests with no tenant resolve to 403, so
// serving the calendar on the control plane needs an explicit
// policy decision — never a silent `public` fallback. Aggregated
// items come ONLY through the `calendar.items_requested` hook —
// this module never reads another module's tables directly, and
// it issues no SQL at all. Control-plane tables (tenants,
// subscriptions, domains, billing, licensing) live in `public`
// and are touched only through their own modules/events, never
// from here.
//
// CONVERGENCE POINT (batch 17 — DONE 2026-11-22): this module
// resolves its tenant through
// `crate::core::tenancy::TenantContext::from_http_request`
// (aliased `TenancyContext`), the same plumbing holidays uses.
// `resolve_tenant` below is still the single convergence point:
// it returns the parsed uuidv7 tenant id plus the `schema_name`
// derived ONLY by `tenancy::schema_name()`, and the `Caller`
// carries that schema name for the future Diesel persistence
// layer (`tenancy::with_tenant_connection`). The migration tables
// (`calendar_feeds`, `calendar_settings`, `calendar_sources`,
// each already carrying a `tenant_id` column) still live in
// `public` until the persistence layer lands; re-pointing them at
// the per-tenant schema is the integrator's step, and no handler,
// record shape, or hook contract changes when it happens:
// `tenant_id` is already on every record and every filter.
//
// Timezones: all timestamps are stored and compared in UTC.
// `chrono-tz` is NOT in Cargo.toml, so IANA zone names (for
// example "Asia/Jakarta") are NOT supported: the `timezone` query
// parameter accepts "UTC"/"Z" or a fixed offset ("+07:00",
// "-05:30", "+0700"). Anything else is a 422 that says so plainly.
// The view response carries `start_local` / `end_local` rendered in
// the requested offset alongside the UTC values.
//
// Non-working days: the configured weekend days (per-company
// setting, ISO weekday numbers 1=Monday..7=Sunday, default
// Saturday+Sunday — weekend days vary by country, so the pair is a
// configurable default, never the only option) plus any holidays
// the `holidays.is_holiday` hook returns for the configured
// holiday country (unset by default: no country configured means
// the hook is skipped and only weekends apply).
//
// Persistence: the live data lives in in-memory DashMap stores
// (the same precedent as the todos, library, and other modules:
// the in-memory store is authoritative). The migration
// `2026-11-22-000095_calendar` defines the persistence targets
// (`calendar_feeds`, `calendar_settings`, `calendar_sources`);
// the tables are not yet the read path and rows do not survive a
// restart until a database-backed implementation is wired in.
// No secret is stored anywhere: feed tokens are HMAC-SHA-256
// signed with a per-process key that never leaves memory, and only
// the SHA-256 hash of an issued token is kept in the registry.
//
// Routes live under `/api/v1/calendar`:
//   GET    /                          — month/week/day/agenda view
//   GET    /items                     — paginated, filtered items
//   POST   /create-intent             — owning-module create link
//   GET    /settings                  — effective settings
//   PUT    /settings                  — save settings
//   GET    /sources                   — per-module visibility toggles
//   PUT    /sources                   — save toggles
//   POST   /feeds                     — issue an iCal feed token
//   GET    /feeds                     — list own feed tokens
//   DELETE /feeds/{id}                — revoke a feed token
//   GET    /feed/{token}              — public iCal subscription feed
//          (".ics" suffix accepted; the token is the credential)
//   POST   /ai/summarize-day          — AI draft daily summary
//   POST   /ai/whats-next             — AI draft briefing
//
// Events emitted on the hook bus:
//   - `calendar.view.requested` (observability; view + range)
//   - `calendar.feed.created`, `calendar.feed.revoked`,
//     `calendar.feed.pruned`
//   - `calendar.settings.updated`, `calendar.sources.updated`
//   - `calendar.create_intent.requested`
//   - `calendar.ai.draft_requested` (see above)
// Events this module listens to:
//   - `calendar.feeds.prune_requested` — prune expired/revoked feeds
//   - `calendar.ai.summarize_day_requested` — grounding for the AI
//     capability runner (`summarize_day`)
//   - `calendar.ai.whats_next_requested` — grounding for the AI
//     capability runner (`whats_next`)
//
// Security: every authenticated handler checks a capability
// through the `role.check` hook and fails closed (no answer means
// not allowed). The public feed endpoint is the only exception:
// it carries no `X-User-Id` header and the signed token itself is
// the credential; unknown, revoked, or expired tokens answer 404
// so token existence never leaks. All input is validated; all
// output goes through the standard JSON envelope; no raw SQL
// (Diesel only, and this module issues no SQL at all yet).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::tenancy::TenantContext as TenancyContext;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::modules::ai::{register_ai_capabilities, AiCapability};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, TimeZone, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Events.
// ---------------------------------------------------------------------------

/// Aggregation contract: emitted with {company_id, user_id, start,
/// end, modules?}; each answering module contributes one array of
/// items (or {"items": [...]}) via `ctx.add_result`.
pub const EVENT_ITEMS_REQUESTED: &str = "calendar.items_requested";
/// Create hand-off: emitted with {module, start, end, title?,
/// user_id, company_id}; the answer carries {"url": ...}.
pub const EVENT_CREATE_REQUESTED: &str = "calendar.create_requested";
/// Draft request to the AI module (same pattern as
/// `todos.ai.draft_requested`).
pub const EVENT_AI_DRAFT_REQUESTED: &str = "calendar.ai.draft_requested";
/// Scheduler-facing listener name for the `calendar.feeds.prune`
/// job kind (not in the scheduler's KNOWN_KINDS yet — see header).
pub const EVENT_FEEDS_PRUNE_REQUESTED: &str = "calendar.feeds.prune_requested";
/// Grounding hooks answered for the AI capability runner.
pub const EVENT_AI_SUMMARIZE_DAY_REQUESTED: &str = "calendar.ai.summarize_day_requested";
pub const EVENT_AI_WHATS_NEXT_REQUESTED: &str = "calendar.ai.whats_next_requested";
/// Holiday lookup consumed from the holidays module (may not exist
/// yet; absence is handled gracefully, never an error).
pub const EVENT_HOLIDAY_LOOKUP: &str = "holidays.is_holiday";
/// Scheduler job kind for feed pruning (documented for the
/// integrator; not in the scheduler's KNOWN_KINDS yet).
pub const SCHEDULER_JOB_KIND_FEEDS_PRUNE: &str = "calendar.feeds.prune";
/// Observability: emitted when a calendar view is served.
pub const EVENT_VIEW_REQUESTED: &str = "calendar.view.requested";
/// Observability: emitted when a create-intent resolves.
pub const EVENT_CREATE_INTENT_REQUESTED: &str = "calendar.create_intent.requested";

// ---------------------------------------------------------------------------
// Permissions (checked through the `role.check` hook; fail closed).
// ---------------------------------------------------------------------------

/// May read calendar views, items, settings, and sources.
pub const PERM_CALENDAR_READ: &str = "calendar:read";
/// May manage feeds, settings, and source toggles.
pub const PERM_CALENDAR_MANAGE: &str = "calendar:manage";
/// May ask the AI module for calendar drafts (enforced again by
/// the AI module itself when it answers the event).
pub const PERM_AI_USE: &str = "ai:use";

// ---------------------------------------------------------------------------
// Limits and configurable defaults.
// ---------------------------------------------------------------------------

/// Views the calendar renders. Configurable default: "month".
pub const CALENDAR_VIEWS: &[&str] = &["month", "week", "day", "agenda"];
/// Default view when the caller does not pick one.
pub const DEFAULT_VIEW: &str = "month";
/// Longest view range the aggregation will serve (days, inclusive).
pub const MAX_VIEW_DAYS: i64 = 370;
/// Longest range the paginated items endpoint will serve (days).
pub const MAX_ITEMS_RANGE_DAYS: i64 = 370;
/// Most modules a caller may ask for in one request.
pub const MAX_MODULE_FILTER: usize = 50;
/// Most items one aggregation returns before pagination applies.
pub const MAX_ITEMS_PER_VIEW: usize = 5_000;
/// Most items the AI context builder grounds on.
pub const MAX_AI_ITEMS: usize = 200;
/// Most days the holiday lookup walks per view request (== range cap).
pub const MAX_HOLIDAY_LOOKUP_DAYS: i64 = 370;
/// Feed token lifetime bounds (days).
pub const MIN_FEED_TTL_DAYS: i64 = 1;
pub const MAX_FEED_TTL_DAYS: i64 = 365;
/// Default feed token lifetime (days) when the caller omits it.
pub const DEFAULT_FEED_TTL_DAYS: i64 = 90;
/// Most feed tokens one user may hold at once.
pub const MAX_FEEDS_PER_USER: usize = 20;
/// Most source toggles one user may store.
pub const MAX_SOURCES_PER_USER: usize = 100;
/// Default working-hours window (24h clock, configurable).
pub const DEFAULT_WORK_START: &str = "09:00";
pub const DEFAULT_WORK_END: &str = "17:00";
/// Default weekend days, ISO weekday numbers 1=Monday..7=Sunday.
/// Saturday (6) + Sunday (7). Configurable per company — weekend
/// days vary by country, so this pair is a default, never the only
/// option.
pub const DEFAULT_WEEKEND_DAYS: &[u8] = &[6, 7];
/// Default item colour when a contributing module sends none.
pub const DEFAULT_ITEM_COLOR: &str = "blue";
/// Item visibility values this module understands.
pub const ITEM_VISIBILITIES: &[&str] = &["private", "shared", "public"];
/// Feed token label length cap.
pub const MAX_FEED_LABEL_LEN: usize = 120;
/// Module id length cap (also used to validate module filters).
pub const MAX_MODULE_ID_LEN: usize = 64;

/// Configurable deep-link templates used when no module answers
/// `calendar.create_requested`. `{start}` and `{end}` are RFC 3339
/// UTC timestamps, `{title}` is URL-encoded when present. These
/// are DEFAULTS, clearly marked: an admin may replace the whole map
/// through settings (`create_link_templates`), and the hook answer
/// always wins over any template.
pub const DEFAULT_CREATE_LINK_TEMPLATES: &[(&str, &str)] = &[
    ("todos", "/todos/new?due_at={start}&title={title}"),
    ("appointments", "/appointments/book?starts_at={start}&ends_at={end}"),
    ("events", "/events/new?starts_at={start}&ends_at={end}&title={title}"),
    ("meetings", "/meetings/new?starts_at={start}&ends_at={end}&title={title}"),
];

// ---------------------------------------------------------------------------
// AI capabilities (registered from `init` via the AI contract).
// ---------------------------------------------------------------------------

pub const CALENDAR_AI_CAPABILITIES: &[AiCapability] = &[
    AiCapability {
        id: "summarize_day",
        module_id: "calendar",
        name: "Summarize day",
        description: "Draft a summary of the caller's calendar day from the aggregated items. Draft only; the AI never creates, moves, or deletes calendar items.",
        input_fields: &["date"],
        allows_pii: false,
        use_case: "calendar.summarize_day",
    },
    AiCapability {
        id: "whats_next",
        module_id: "calendar",
        name: "What's next",
        description: "Draft a short briefing of what comes next on the caller's calendar from the aggregated items. Draft only; the AI never creates, moves, or deletes calendar items.",
        input_fields: &["date"],
        allows_pii: false,
        use_case: "calendar.whats_next",
    },
];

// ---------------------------------------------------------------------------
// Shared item shape.
// ---------------------------------------------------------------------------

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
    /// `aggregate_items`); absent means unscoped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    /// Start rendered in the viewer's timezone (view responses
    /// only; never supplied by contributing modules).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_local: Option<String>,
    /// End rendered in the viewer's timezone (view responses only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_local: Option<String>,
}

fn default_visibility() -> String {
    "shared".to_string()
}

impl CalendarItem {
    /// Effective end for overlap math: the stated end, else the
    /// start plus one hour (an all-day item with no end spans its
    /// start day: start plus 24 hours).
    fn effective_end(&self) -> DateTime<Utc> {
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
    fn with_local_times(&self, offset: FixedOffset) -> CalendarItem {
        let mut item = self.clone();
        item.start_local = Some(self.start.with_timezone(&offset).to_rfc3339());
        item.end_local = Some(self.effective_end().with_timezone(&offset).to_rfc3339());
        if item.color.is_none() {
            item.color = Some(DEFAULT_ITEM_COLOR.to_string());
        }
        item
    }

    /// Public JSON view of one item (local times already applied).
    fn to_json(&self) -> Value {
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
/// intersect. Hints only — the calendar never blocks a creation.
#[derive(Debug, Clone, Serialize)]
pub struct ConflictHint {
    pub first_module: String,
    pub first_id: String,
    pub second_module: String,
    pub second_id: String,
    pub overlap_start: String,
    pub overlap_end: String,
}

// ---------------------------------------------------------------------------
// Stored records (in-memory authoritative store; the migration
// defines the persistence targets).
// ---------------------------------------------------------------------------

/// A revocable iCal subscription feed. Only the SHA-256 hash of
/// the issued token is stored — never the raw token.
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

fn default_work_start() -> String {
    DEFAULT_WORK_START.to_string()
}

fn default_work_end() -> String {
    DEFAULT_WORK_END.to_string()
}

fn default_view_string() -> String {
    DEFAULT_VIEW.to_string()
}

fn default_timezone_string() -> String {
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

// ---------------------------------------------------------------------------
// Request / response shapes.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CalendarViewQuery {
    view: Option<String>,
    from: Option<String>,
    to: Option<String>,
    modules: Option<String>,
    timezone: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CalendarItemsQuery {
    from: Option<String>,
    to: Option<String>,
    modules: Option<String>,
    timezone: Option<String>,
    #[serde(flatten)]
    pagination: PaginationParams,
}

#[derive(Debug, Deserialize)]
struct CreateIntentRequest {
    module: String,
    start: String,
    end: Option<String>,
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateSettingsRequest {
    scope: Option<String>,
    weekend_days: Option<Vec<u8>>,
    working_hours_start: Option<String>,
    working_hours_end: Option<String>,
    default_view: Option<String>,
    timezone: Option<String>,
    holiday_country: Option<String>,
    create_link_templates: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
struct UpdateSourcesRequest {
    sources: Vec<SourceToggleInput>,
}

#[derive(Debug, Deserialize)]
struct SourceToggleInput {
    module: String,
    visible: bool,
}

#[derive(Debug, Deserialize)]
struct CreateFeedRequest {
    modules: Option<Vec<String>>,
    expires_in_days: Option<i64>,
    label: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AiDraftRequest {
    date: Option<String>,
}

#[derive(Debug, Serialize)]
struct AiDraftResponse {
    kind: String,
    draft: String,
    ai_generated: bool,
    status: String,
    item_ids: Vec<String>,
    user_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    requested_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Shared state.
// ---------------------------------------------------------------------------

/// In-memory stores for the calendar. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers use
/// `ctx.hooks` directly. Durable rows for these types are defined
/// by migration `2026-11-22-000095_calendar`; until that migration
/// is wired into a database-backed implementation, feeds,
/// settings, and sources do not survive a restart.
#[derive(Clone)]
pub struct CalendarState {
    /// Keyed by feed id.
    pub feeds: Arc<DashMap<Uuid, CalendarFeed>>,
    /// Company-level settings, keyed by `"{tenant_id}:{company_id}"`.
    pub company_settings: Arc<DashMap<String, CalendarSettings>>,
    /// User-level settings, keyed by `"{tenant_id}:{user_id}"`.
    pub user_settings: Arc<DashMap<String, CalendarSettings>>,
    /// Keyed by `"{tenant_id}:{user_id}:{module}"`.
    pub sources: Arc<DashMap<String, CalendarSource>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
    /// Per-process HMAC signing key for feed tokens. Generated
    /// from two random UUIDs (32 bytes), same approach as the
    /// library module: tokens issued by this process cannot be
    /// forged without the key, and the key never leaves memory.
    pub signing_key: Arc<RwLock<[u8; 32]>>,
}

impl Default for CalendarState {
    fn default() -> Self {
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let mut key = [0u8; 32];
        key[..16].copy_from_slice(first.as_bytes());
        key[16..].copy_from_slice(second.as_bytes());
        Self {
            feeds: Arc::new(DashMap::new()),
            company_settings: Arc::new(DashMap::new()),
            user_settings: Arc::new(DashMap::new()),
            sources: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
            signing_key: Arc::new(RwLock::new(key)),
        }
    }
}

impl CalendarState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// HMAC-SHA-256 signature hex for a feed id + expiry, same
    /// token form as the library module.
    fn sign_token(&self, feed_id: &str, expiry: i64) -> String {
        let key = *self.signing_key.read().expect("signing key lock poisoned");
        let payload = format!("{feed_id}:{expiry}");
        let mut mac = Hmac::<Sha256>::new_from_slice(&key)
            .expect("HMAC key is always a valid length");
        mac.update(payload.as_bytes());
        let tag = mac.finalize().into_bytes();
        tag.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// SHA-256 hex of a full issued token (the only form stored).
    fn hash_token(token: &str) -> String {
        let digest = Sha256::digest(token.as_bytes());
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// Issue a signed feed token for a feed id. Returns the token
    /// string (shown to the caller exactly once) and its expiry.
    fn issue_token(&self, feed_id: Uuid, expires_secs: i64) -> (String, DateTime<Utc>) {
        let expires_at = Utc::now() + chrono::Duration::seconds(expires_secs);
        let expiry = expires_at.timestamp();
        let signature = self.sign_token(&feed_id.to_string(), expiry);
        (format!("{feed_id}.{expiry}.{signature}"), expires_at)
    }
}

/// Constant-time string comparison, so token verification does
/// not leak how many leading characters matched.
fn constant_time_eq(expected: &str, actual: &str) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    expected
        .as_bytes()
        .iter()
        .zip(actual.as_bytes().iter())
        .fold(0u8, |acc, (left, right)| acc | (left ^ right))
        == 0
}

// ---------------------------------------------------------------------------
// Helpers: caller, capabilities, validation.
// ---------------------------------------------------------------------------

/// The tenant context for one request, resolved ONLY through
/// `crate::core::tenancy::TenantContext::from_http_request`
/// (aliased `TenancyContext` — the batch-17 tenancy plumbing,
/// parsed uuidv7 tenant id plus the `schema_name` derived ONLY by
/// `tenancy::schema_name()`). This is the single convergence point:
/// nothing else in this file derives a schema name, and no other
/// function resolves a tenant. Fail closed:
/// `from_http_request` answers 401 when the request did not
/// resolve to a tenant and 403 when the resolved tenant id is not
/// a valid UUID; no resolved tenant means no calendar access at
/// all, never a silent fall back to `public`. OPEN: control-plane
/// requests with no tenant get 403 here — serving the calendar on
/// the control plane needs an explicit policy decision, never a
/// silent `public` fallback.
fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<TenancyContext> {
    TenancyContext::from_http_request(req)
}

/// The caller, from the headers every module in this codebase uses:
/// `X-User-Id` identifies the actor, `X-Role` carries the role slug
/// the capability check runs against. (Same shape as `todos.rs`.)
/// The tenant id, schema name, and company id come from the
/// resolved tenant context, never from a client header.
/// `schema_name` is carried (not just derived on demand) so the
/// future Diesel persistence layer takes it from exactly one place.
struct Caller {
    actor: String,
    role: String,
    tenant_id: String,
    /// Derived ONLY by `tenancy::schema_name()` (see
    /// `resolve_tenant`). Unread until the Diesel persistence layer
    /// lands and takes it into `tenancy::with_tenant_connection`.
    #[allow(dead_code)]
    schema_name: String,
    /// Company id from the tenant context; empty means unscoped.
    company_id: Option<String>,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let tenant = resolve_tenant(req)?;
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller {
        actor,
        role,
        tenant_id: tenant.tenant_id.to_string(),
        schema_name: tenant.schema_name.clone(),
        company_id: tenant.company_id.clone(),
    })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

/// A module id is a lowercase identifier: letters, digits, dash,
/// underscore only. Anything else is rejected so a filter can
/// never smuggle a path or query fragment into a hook payload.
fn validate_module_id(value: &str) -> UnifiedResult<String> {
    let cleaned = value.trim().to_lowercase();
    if cleaned.is_empty() || cleaned.len() > MAX_MODULE_ID_LEN {
        return Err(UnifiedError::bad_request(
            "module id must be 1-64 characters",
        ));
    }
    if !cleaned
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(UnifiedError::bad_request(format!(
            "Invalid module id \"{cleaned}\": expected letters, digits, dash, or underscore"
        )));
    }
    Ok(cleaned)
}

/// Parse a comma-separated `modules` query value into a validated,
/// de-duplicated list.
fn parse_module_filter(raw: Option<&str>) -> UnifiedResult<Option<Vec<String>>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let mut out: Vec<String> = Vec::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let cleaned = validate_module_id(part)?;
        if !out.contains(&cleaned) {
            out.push(cleaned);
        }
    }
    if out.len() > MAX_MODULE_FILTER {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_MODULE_FILTER} modules may be requested at once"
        )));
    }
    Ok(Some(out))
}

/// Parse an RFC 3339 timestamp (UTC or with an offset; converted
/// to UTC for comparison).
fn parse_time(value: &str, field: &str) -> UnifiedResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.trim())
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| {
            UnifiedError::bad_request(format!(
                "{field} must be an RFC 3339 timestamp, for example 2026-11-23T02:00:00Z"
            ))
        })
}

/// Parse the `timezone` parameter. Accepts "UTC"/"Z" or a fixed
/// offset ("+07:00", "-05:30", "+0700"). IANA names are rejected
/// with a plain explanation: `chrono-tz` is not in Cargo.toml, so
/// named zones cannot be resolved in this build.
fn parse_timezone(raw: Option<&str>) -> UnifiedResult<FixedOffset> {
    let Some(raw) = raw else {
        return Ok(FixedOffset::east_opt(0).expect("zero offset is always valid"));
    };
    let cleaned = raw.trim();
    if cleaned.eq_ignore_ascii_case("utc")
        || cleaned.eq_ignore_ascii_case("z")
        || cleaned.eq_ignore_ascii_case("etc/utc")
    {
        return Ok(FixedOffset::east_opt(0).expect("zero offset is always valid"));
    }
    let digits = cleaned.replace(':', "");
    if (digits.len() == 5 || digits.len() == 6)
        && (digits.starts_with('+') || digits.starts_with('-'))
        && digits[1..].chars().all(|ch| ch.is_ascii_digit())
    {
        let sign: i32 = if digits.starts_with('-') { -1 } else { 1 };
        let hours: i32 = digits[1..3].parse().map_err(|_| {
            UnifiedError::bad_request("Invalid timezone offset")
        })?;
        let minutes: i32 = digits[3..5].parse().map_err(|_| {
            UnifiedError::bad_request("Invalid timezone offset")
        })?;
        if hours > 14 || minutes > 59 {
            return Err(UnifiedError::bad_request(
                "Timezone offset hours must be at most 14 and minutes at most 59",
            ));
        }
        return FixedOffset::east_opt(sign * (hours * 3600 + minutes * 60)).ok_or_else(|| {
            UnifiedError::bad_request("Timezone offset is out of range")
        });
    }
    Err(UnifiedError::unprocessable(format!(
        "Timezone \"{cleaned}\" is not supported: pass \"UTC\" or a fixed offset such as \"+07:00\". IANA zone names need the chrono-tz crate, which is not in Cargo.toml."
    )))
}

/// Validate a "HH:MM" 24-hour clock value.
fn validate_clock(value: &str, field: &str) -> UnifiedResult<String> {
    let cleaned = value.trim();
    let parts: Vec<&str> = cleaned.split(':').collect();
    if parts.len() != 2 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a 24-hour clock value such as \"09:00\""
        )));
    }
    let hours: u32 = parts[0].parse().map_err(|_| {
        UnifiedError::bad_request(format!("{field} must be a 24-hour clock value"))
    })?;
    let minutes: u32 = parts[1].parse().map_err(|_| {
        UnifiedError::bad_request(format!("{field} must be a 24-hour clock value"))
    })?;
    if hours > 23 || minutes > 59 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a 24-hour clock value between \"00:00\" and \"23:59\""
        )));
    }
    Ok(format!("{hours:02}:{minutes:02}"))
}

/// Validate an ISO 3166-1 alpha-2 country code (two ASCII
/// letters), or an empty string meaning "no country configured".
fn validate_country(value: &str) -> UnifiedResult<String> {
    let cleaned = value.trim().to_uppercase();
    if cleaned.is_empty() {
        return Ok(String::new());
    }
    if cleaned.len() == 2 && cleaned.chars().all(|ch| ch.is_ascii_alphabetic()) {
        return Ok(cleaned);
    }
    Err(UnifiedError::bad_request(
        "holiday_country must be an ISO 3166-1 alpha-2 code such as \"ID\", or empty",
    ))
}

/// Validate weekend days: ISO weekday numbers 1=Monday..7=Sunday,
/// at least one day, at most six, de-duplicated and sorted.
fn validate_weekend_days(days: &[u8]) -> UnifiedResult<Vec<u8>> {
    if days.is_empty() || days.len() > 6 {
        return Err(UnifiedError::bad_request(
            "weekend_days must contain 1 to 6 ISO weekday numbers (1=Monday..7=Sunday)",
        ));
    }
    let mut cleaned: Vec<u8> = Vec::new();
    for day in days {
        if !(1..=7).contains(day) {
            return Err(UnifiedError::bad_request(format!(
                "weekend_days entry {day} is out of range: expected 1 (Monday) to 7 (Sunday)"
            )));
        }
        if !cleaned.contains(day) {
            cleaned.push(*day);
        }
    }
    cleaned.sort_unstable();
    Ok(cleaned)
}

/// URL-encode a short value for interpolation into a deep-link
/// template (percent-encoding; only unreserved characters pass).
fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Aggregation.
// ---------------------------------------------------------------------------

/// Effective settings for a caller: company defaults overlaid
/// with the caller's own overrides (a set user value wins).
/// Lookups are tenant-scoped: keys are
/// `"{tenant_id}:{company_id}"` and `"{tenant_id}:{user_id}"`, so
/// settings can never leak across tenants.
fn effective_settings(state: &CalendarState, caller: &Caller) -> CalendarSettings {
    let mut settings = CalendarSettings::default();
    if let Some(company_id) = &caller.company_id {
        let key = format!("{}:{company_id}", caller.tenant_id);
        if let Some(company) = state.company_settings.get(&key) {
            settings = company.clone();
        }
    }
    let user_key = format!("{}:{}", caller.tenant_id, caller.actor);
    if let Some(user) = state.user_settings.get(&user_key) {
        let user = user.clone();
        settings.weekend_days = user.weekend_days;
        settings.working_hours_start = user.working_hours_start;
        settings.working_hours_end = user.working_hours_end;
        settings.default_view = user.default_view;
        settings.timezone = user.timezone;
        settings.holiday_country = user.holiday_country;
        if !user.create_link_templates.is_empty() {
            settings.create_link_templates = user.create_link_templates;
        }
    }
    settings
}

/// Modules the caller has hidden through source toggles.
/// Tenant-scoped: only toggles whose tenant_id matches the
/// caller's tenant apply.
fn hidden_modules(state: &CalendarState, caller: &Caller) -> HashSet<String> {
    let mut hidden = HashSet::new();
    for entry in state.sources.iter() {
        let source = entry.value();
        if source.tenant_id == caller.tenant_id
            && source.user_id == caller.actor
            && !source.visible
        {
            hidden.insert(source.module.clone());
        }
    }
    hidden
}

/// Ask every module for items in [start, end] and merge the
/// answers. Returns the visible items (sorted by start,
/// de-duplicated by (module, id)) and the number of contributed
/// items skipped as malformed.
fn aggregate_items(
    ctx: &AppContext,
    state: &CalendarState,
    caller: &Caller,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    module_filter: Option<&[String]>,
) -> (Vec<CalendarItem>, usize) {
    let mut request = json!({
        "tenant_id": caller.tenant_id,
        "company_id": caller.company_id,
        "user_id": caller.actor,
        "start": start.to_rfc3339(),
        "end": end.to_rfc3339(),
    });
    if let Some(modules) = module_filter {
        request["modules"] = json!(modules);
    }
    let answer = ctx.hooks.emit(EVENT_ITEMS_REQUESTED, request);
    let hidden = hidden_modules(state, caller);
    let mut items: Vec<CalendarItem> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut skipped = 0usize;
    for result in &answer.results {
        let candidates: Vec<Value> = match result {
            Value::Array(values) => values.clone(),
            Value::Object(map) => match map.get("items") {
                Some(Value::Array(values)) => values.clone(),
                _ => vec![result.clone()],
            },
            _ => {
                skipped += 1;
                continue;
            }
        };
        for candidate in candidates {
            let item: CalendarItem = match serde_json::from_value(candidate) {
                Ok(item) => item,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            if item.title.trim().is_empty() || item.module.trim().is_empty() {
                skipped += 1;
                continue;
            }
            if item.start < start || item.start > end {
                continue;
            }
            if let Some(modules) = module_filter {
                if !modules.contains(&item.module) {
                    continue;
                }
            }
            if hidden.contains(&item.module) {
                continue;
            }
            // Visibility, enforced server-side: private items reach
            // only their owner.
            if item.visibility == "private" {
                match &item.owner_user_id {
                    Some(owner) if owner == &caller.actor => {}
                    _ => continue,
                }
            }
            // Tenancy, enforced server-side by exclusion: an item
            // scoped to another company never leaks (no 403/404
            // signal, it is simply absent). Same rule for tenant:
            // an item carrying a tenant_id different from the
            // caller's tenant is excluded silently.
            if let Some(item_tenant) = &item.tenant_id {
                if item_tenant != &caller.tenant_id {
                    continue;
                }
            }
            match (&item.company_id, &caller.company_id) {
                (Some(item_company), Some(caller_company)) => {
                    if item_company != caller_company {
                        continue;
                    }
                }
                (Some(_), None) => continue,
                _ => {}
            }
            if item.end.is_some_and(|item_end| item_end < item.start) {
                skipped += 1;
                continue;
            }
            let key = (item.module.clone(), item.id.clone());
            if !seen.insert(key) {
                continue;
            }
            items.push(item);
        }
    }
    items.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| left.module.cmp(&right.module))
            .then_with(|| left.id.cmp(&right.id))
    });
    if items.len() > MAX_ITEMS_PER_VIEW {
        items.truncate(MAX_ITEMS_PER_VIEW);
    }
    (items, skipped)
}

/// Non-working days inside [start_date, end_date]: configured
/// weekend days plus holidays answered by the `holidays.is_holiday`
/// hook. When no handler is registered for the hook (the holidays
/// module may not exist yet) or no holiday country is configured,
/// only weekends apply — never an error.
fn non_working_days(
    ctx: &AppContext,
    tenant_id: &str,
    settings: &CalendarSettings,
    start_date: NaiveDate,
    end_date: NaiveDate,
) -> Vec<NonWorkingDay> {
    let weekend: HashSet<u8> = settings.weekend_days.iter().copied().collect();
    let mut days: Vec<NonWorkingDay> = Vec::new();
    let mut date = start_date;
    let mut walked: i64 = 0;
    while date <= end_date && walked < MAX_HOLIDAY_LOOKUP_DAYS {
        walked += 1;
        let iso_weekday = date.weekday().number_from_monday() as u8;
        if weekend.contains(&iso_weekday) {
            days.push(NonWorkingDay {
                date: date.to_string(),
                kind: "weekend".to_string(),
                name: None,
                source: None,
            });
        } else if !settings.holiday_country.trim().is_empty() {
            let answer = ctx.hooks.emit(
                EVENT_HOLIDAY_LOOKUP,
                json!({
                    "date": date.to_string(),
                    "country": settings.holiday_country.trim().to_uppercase(),
                    // Required by the holidays module: without a tenant id
                    // its handler answers nothing (fail closed).
                    "tenant_id": tenant_id,
                }),
            );
            for result in &answer.results {
                let is_holiday = result
                    .get("is_holiday")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if is_holiday {
                    days.push(NonWorkingDay {
                        date: date.to_string(),
                        kind: "holiday".to_string(),
                        name: result
                            .get("name")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        source: result
                            .get("source")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    });
                    break;
                }
            }
        }
        date = date + chrono::Duration::days(1);
    }
    days
}

/// Overlap hints for one user's items: pairs whose intervals
/// intersect. Hints only — never a hard block.
fn conflict_hints(items: &[CalendarItem]) -> Vec<ConflictHint> {
    let mut hints: Vec<ConflictHint> = Vec::new();
    for left_index in 0..items.len() {
        let left = &items[left_index];
        let Some(left_owner) = &left.owner_user_id else {
            continue;
        };
        for right in items.iter().skip(left_index + 1) {
            if right.owner_user_id.as_deref() != Some(left_owner.as_str()) {
                continue;
            }
            let left_end = left.effective_end();
            let right_end = right.effective_end();
            if left.start < right_end && right.start < left_end {
                let overlap_start = left.start.max(right.start);
                let overlap_end = left_end.min(right_end);
                hints.push(ConflictHint {
                    first_module: left.module.clone(),
                    first_id: left.id.clone(),
                    second_module: right.module.clone(),
                    second_id: right.id.clone(),
                    overlap_start: overlap_start.to_rfc3339(),
                    overlap_end: overlap_end.to_rfc3339(),
                });
            }
        }
    }
    hints
}

/// Default view range for a view kind, in UTC, from "now".
fn default_range(view: &str, now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let day_start = now.date_naive().and_hms_opt(0, 0, 0).expect("midnight is valid");
    let day_start = Utc.from_utc_datetime(&day_start);
    match view {
        "day" => (day_start, day_start + chrono::Duration::days(1)),
        "week" => {
            let monday_offset = now.weekday().num_days_from_monday() as i64;
            let monday = day_start - chrono::Duration::days(monday_offset);
            (monday, monday + chrono::Duration::days(7))
        }
        "agenda" => (day_start, day_start + chrono::Duration::days(14)),
        _ => {
            let first = now
                .date_naive()
                .with_day(1)
                .expect("first of month is valid");
            let first = Utc.from_utc_datetime(
                &first.and_hms_opt(0, 0, 0).expect("midnight is valid"),
            );
            let next_month = if now.month() == 12 {
                NaiveDate::from_ymd_opt(now.year() + 1, 1, 1).expect("valid date")
            } else {
                NaiveDate::from_ymd_opt(now.year(), now.month() + 1, 1).expect("valid date")
            };
            let next_month = Utc.from_utc_datetime(
                &next_month
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight is valid"),
            );
            (first, next_month)
        }
    }
}

// ---------------------------------------------------------------------------
// iCalendar (RFC 5545, hand-built — no calendar crate in Cargo.toml).
// Follows the manual building pattern from events.rs / appointments.rs.
// ---------------------------------------------------------------------------

/// Escape a TEXT value per RFC 5545 section 3.3.11: backslash,
/// semicolon, comma, and newlines. Folding is applied at 75
/// octets per line (section 3.1) with a single leading space on
/// continuation lines.
fn ics_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            other => out.push(other),
        }
    }
    out
}

fn ics_fold(line: &str) -> String {
    if line.len() <= 75 {
        return line.to_string();
    }
    let mut out = String::new();
    let mut current = String::new();
    for ch in line.chars() {
        if current.len() + ch.len_utf8() > 75 {
            out.push_str(&current);
            out.push_str("\r\n ");
            current.clear();
        }
        current.push(ch);
    }
    out.push_str(&current);
    out
}

fn ics_datetime(value: DateTime<Utc>) -> String {
    value.format("%Y%m%dT%H%M%SZ").to_string()
}

fn ics_date(value: DateTime<Utc>) -> String {
    value.format("%Y%m%d").to_string()
}

fn ics_item_block(item: &CalendarItem) -> String {
    let mut out = String::new();
    out.push_str("BEGIN:VEVENT\r\n");
    out.push_str(&ics_fold(&format!(
        "UID:{}-{}@unified",
        ics_escape(&item.module),
        ics_escape(&item.id)
    )));
    out.push_str("\r\n");
    out.push_str(&format!("DTSTAMP:{}\r\n", ics_datetime(Utc::now())));
    if item.all_day {
        out.push_str(&format!("DTSTART;VALUE=DATE:{}\r\n", ics_date(item.start)));
        out.push_str(&format!(
            "DTEND;VALUE=DATE:{}\r\n",
            ics_date(item.effective_end())
        ));
    } else {
        out.push_str(&format!("DTSTART:{}\r\n", ics_datetime(item.start)));
        out.push_str(&format!(
            "DTEND:{}\r\n",
            ics_datetime(item.effective_end())
        ));
    }
    out.push_str(&ics_fold(&format!("SUMMARY:{}", ics_escape(&item.title))));
    out.push_str("\r\n");
    if let Some(label) = &item.label {
        out.push_str(&ics_fold(&format!(
            "CATEGORIES:{}",
            ics_escape(label)
        )));
        out.push_str("\r\n");
    }
    if let Some(link) = &item.deep_link {
        out.push_str(&ics_fold(&format!("URL:{}", ics_escape(link))));
        out.push_str("\r\n");
        out.push_str(&ics_fold(&format!(
            "DESCRIPTION:{}",
            ics_escape(link)
        )));
        out.push_str("\r\n");
    }
    out.push_str(&format!(
        "X-UNIFIED-MODULE:{}\r\n",
        ics_escape(&item.module)
    ));
    out.push_str(&format!(
        "X-UNIFIED-VISIBILITY:{}\r\n",
        ics_escape(&item.visibility)
    ));
    out.push_str("END:VEVENT");
    out
}

fn ics_calendar(items: &[CalendarItem], calendar_name: &str) -> String {
    let mut out = String::new();
    out.push_str("BEGIN:VCALENDAR\r\n");
    out.push_str("VERSION:2.0\r\n");
    out.push_str("PRODID:-//Unified//Calendar//EN\r\n");
    out.push_str("CALSCALE:GREGORIAN\r\n");
    out.push_str("METHOD:PUBLISH\r\n");
    out.push_str(&ics_fold(&format!(
        "X-WR-CALNAME:{}",
        ics_escape(calendar_name)
    )));
    out.push_str("\r\n");
    for item in items {
        out.push_str(&ics_item_block(item));
        out.push_str("\r\n");
    }
    out.push_str("END:VCALENDAR\r\n");
    out
}

fn ics_response(body: String) -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/calendar; charset=utf-8")
        .insert_header(("Content-Disposition", "attachment; filename=\"calendar.ics\""))
        .body(body)
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

/// GET /api/v1/calendar — month/week/day/agenda view: aggregated
/// items (with viewer-local times), non-working days, and conflict
/// hints. Empty when no module answers; never an error.
async fn get_calendar_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
    query: web::Query<CalendarViewQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_READ)?;
        let settings = effective_settings(&state, &caller);
        let view = query
            .view
            .clone()
            .unwrap_or_else(|| settings.default_view.clone());
        if !CALENDAR_VIEWS.contains(&view.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "view must be one of {}",
                CALENDAR_VIEWS.join(", ")
            )));
        }
        let offset = parse_timezone(
            query
                .timezone
                .as_deref()
                .or(Some(settings.timezone.as_str())),
        )?;
        let now = Utc::now();
        let (default_from, default_to) = default_range(&view, now);
        let from = match &query.from {
            Some(raw) => parse_time(raw, "from")?,
            None => default_from,
        };
        let to = match &query.to {
            Some(raw) => parse_time(raw, "to")?,
            None => default_to,
        };
        if to <= from {
            return Err(UnifiedError::bad_request("to must be after from"));
        }
        if (to - from).num_days() > MAX_VIEW_DAYS {
            return Err(UnifiedError::bad_request(format!(
                "View range must be at most {MAX_VIEW_DAYS} days"
            )));
        }
        let modules = parse_module_filter(query.modules.as_deref())?;
        let (items, skipped) = aggregate_items(
            &ctx,
            &state,
            &caller,
            from,
            to,
            modules.as_deref(),
        );
        let rendered: Vec<Value> = items
            .iter()
            .map(|item| item.with_local_times(offset).to_json())
            .collect();
        let non_working = non_working_days(
            &ctx,
            &caller.tenant_id,
            &settings,
            from.date_naive(),
            to.date_naive(),
        );
        let conflicts = conflict_hints(&items);
        state.emit(
            EVENT_VIEW_REQUESTED,
            json!({
                "tenant_id": caller.tenant_id,
                "user_id": caller.actor,
                "view": view,
                "from": from.to_rfc3339(),
                "to": to.to_rfc3339(),
                "item_count": items.len(),
            }),
        );
        Ok(json!({
            "view": view,
            "from": from.to_rfc3339(),
            "to": to.to_rfc3339(),
            "timezone": query.timezone.clone().unwrap_or(settings.timezone.clone()),
            "items": rendered,
            "item_count": items.len(),
            "skipped_malformed_items": skipped,
            "non_working_days": non_working,
            "conflicts": conflicts,
            "working_hours": {
                "start": settings.working_hours_start,
                "end": settings.working_hours_end,
            },
            "weekend_days": settings.weekend_days,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// GET /api/v1/calendar/items — the same aggregation, paginated
/// and filtered. `from`/`to` default to the current month.
async fn list_calendar_items(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
    query: web::Query<CalendarItemsQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_READ)?;
        let settings = effective_settings(&state, &caller);
        let offset = parse_timezone(
            query
                .timezone
                .as_deref()
                .or(Some(settings.timezone.as_str())),
        )?;
        let now = Utc::now();
        let (default_from, default_to) = default_range("month", now);
        let from = match &query.from {
            Some(raw) => parse_time(raw, "from")?,
            None => default_from,
        };
        let to = match &query.to {
            Some(raw) => parse_time(raw, "to")?,
            None => default_to,
        };
        if to <= from {
            return Err(UnifiedError::bad_request("to must be after from"));
        }
        if (to - from).num_days() > MAX_ITEMS_RANGE_DAYS {
            return Err(UnifiedError::bad_request(format!(
                "Items range must be at most {MAX_ITEMS_RANGE_DAYS} days"
            )));
        }
        let modules = parse_module_filter(query.modules.as_deref())?;
        let (items, skipped) = aggregate_items(
            &ctx,
            &state,
            &caller,
            from,
            to,
            modules.as_deref(),
        );
        let rendered: Vec<CalendarItem> = items
            .iter()
            .map(|item| item.with_local_times(offset))
            .collect();
        let page = paginate(rendered, &query.pagination);
        let mut data = page.to_json();
        data["skipped_malformed_items"] = json!(skipped);
        Ok(data)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// POST /api/v1/calendar/create-intent — resolve the owning
/// module's create deep link for a date/time. The calendar NEVER
/// creates a record itself.
async fn create_intent(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
    body: web::Json<CreateIntentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_READ)?;
        let input = body.into_inner();
        let module = validate_module_id(&input.module)?;
        let start = parse_time(&input.start, "start")?;
        let end = match &input.end {
            Some(raw) => Some(parse_time(raw, "end")?),
            None => None,
        };
        if let Some(end) = end {
            if end <= start {
                return Err(UnifiedError::bad_request("end must be after start"));
            }
        }
        let title = match &input.title {
            Some(title) if title.trim().is_empty() => None,
            Some(title) if title.chars().count() > 200 => {
                return Err(UnifiedError::bad_request(
                    "title must be at most 200 characters",
                ));
            }
            Some(title) => Some(title.trim().to_string()),
            None => None,
        };
        let answer = ctx.hooks.emit(
            EVENT_CREATE_REQUESTED,
            json!({
                "tenant_id": caller.tenant_id,
                "module": module,
                "start": start.to_rfc3339(),
                "end": end.map(|value| value.to_rfc3339()),
                "title": title,
                "user_id": caller.actor,
                "company_id": caller.company_id,
            }),
        );
        for result in &answer.results {
            if let Some(url) = result.get("url").and_then(Value::as_str) {
                if !url.trim().is_empty() {
                    state.emit(
                        EVENT_CREATE_INTENT_REQUESTED,
                        json!({
                            "tenant_id": caller.tenant_id,
                            "module": module,
                            "url": url,
                            "user_id": caller.actor,
                        }),
                    );
                    return Ok(json!({
                        "module": module,
                        "url": url,
                        "start": start.to_rfc3339(),
                        "end": end.map(|value| value.to_rfc3339()),
                        "source": "module_hook",
                    }));
                }
            }
        }
        // Fallback: a configurable deep-link template (admin-set
        // templates win over the built-in defaults; the hook answer
        // above always wins over both).
        let settings = effective_settings(&state, &caller);
        let template = settings
            .create_link_templates
            .get(&module)
            .cloned()
            .or_else(|| {
                DEFAULT_CREATE_LINK_TEMPLATES
                    .iter()
                    .find(|(name, _)| *name == module)
                    .map(|(_, template)| (*template).to_string())
            });
        match template {
            Some(template) => {
                let url = template
                    .replace(
                        "{start}",
                        &url_encode(&start.to_rfc3339()),
                    )
                    .replace(
                        "{end}",
                        &url_encode(
                            &end.unwrap_or(start + chrono::Duration::hours(1))
                                .to_rfc3339(),
                        ),
                    )
                    .replace("{title}", &url_encode(title.as_deref().unwrap_or("")));
                Ok(json!({
                    "module": module,
                    "url": url,
                    "start": start.to_rfc3339(),
                    "end": end.map(|value| value.to_rfc3339()),
                    "source": "template",
                }))
            }
            None => Err(UnifiedError::new(
                format!(
                    "No owning module answered calendar.create_requested for module \"{module}\" and no create-link template is configured for it"
                ),
                503,
            )),
        }
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// GET /api/v1/calendar/settings — effective settings (company
/// defaults merged with the caller's overrides).
async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CalendarSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_READ)?;
        Ok(effective_settings(&state, &caller))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// PUT /api/v1/calendar/settings — save settings. `scope` is
/// "user" (default) or "company"; both are tenant-scoped.
async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CalendarSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_MANAGE)?;
        let input = body.into_inner();
        let scope = input.scope.as_deref().unwrap_or("user");
        if scope != "user" && scope != "company" {
            return Err(UnifiedError::bad_request(
                "scope must be \"user\" or \"company\"",
            ));
        }
        let mut settings = if scope == "company" {
            match &caller.company_id {
                Some(company_id) => {
                    let key = format!("{}:{company_id}", caller.tenant_id);
                    state
                        .company_settings
                        .get(&key)
                        .map(|entry| entry.clone())
                        .unwrap_or_default()
                }
                None => CalendarSettings::default(),
            }
        } else {
            let key = format!("{}:{}", caller.tenant_id, caller.actor);
            state
                .user_settings
                .get(&key)
                .map(|entry| entry.clone())
                .unwrap_or_default()
        };
        if let Some(days) = &input.weekend_days {
            settings.weekend_days = validate_weekend_days(days)?;
        }
        if let Some(start) = &input.working_hours_start {
            settings.working_hours_start = validate_clock(start, "working_hours_start")?;
        }
        if let Some(end) = &input.working_hours_end {
            settings.working_hours_end = validate_clock(end, "working_hours_end")?;
        }
        if let Some(view) = &input.default_view {
            if !CALENDAR_VIEWS.contains(&view.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "default_view must be one of {}",
                    CALENDAR_VIEWS.join(", ")
                )));
            }
            settings.default_view = view.clone();
        }
        if let Some(timezone) = &input.timezone {
            // Validated only; the parsed offset is not stored.
            parse_timezone(Some(timezone))?;
            settings.timezone = timezone.trim().to_string();
        }
        if let Some(country) = &input.holiday_country {
            settings.holiday_country = validate_country(country)?;
        }
        if let Some(templates) = &input.create_link_templates {
            if templates.len() > MAX_MODULE_FILTER {
                return Err(UnifiedError::bad_request(format!(
                    "At most {MAX_MODULE_FILTER} create-link templates may be stored"
                )));
            }
            let mut cleaned: HashMap<String, String> = HashMap::new();
            for (module, template) in templates {
                let module = validate_module_id(module)?;
                if template.len() > 512 {
                    return Err(UnifiedError::bad_request(
                        "A create-link template must be at most 512 characters",
                    ));
                }
                cleaned.insert(module, template.clone());
            }
            settings.create_link_templates = cleaned;
        }
        if scope == "company" {
            let company_id = caller.company_id.clone().ok_or_else(|| {
                UnifiedError::bad_request(
                    "Company scope needs a company on the tenant context",
                )
            })?;
            let key = format!("{}:{company_id}", caller.tenant_id);
            state.company_settings.insert(key, settings.clone());
        } else {
            let key = format!("{}:{}", caller.tenant_id, caller.actor);
            state.user_settings.insert(key, settings.clone());
        }
        state.emit(
            "calendar.settings.updated",
            json!({
                "tenant_id": caller.tenant_id,
                "user_id": caller.actor,
                "scope": scope,
            }),
        );
        Ok(settings)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// GET /api/v1/calendar/sources — the caller's per-module
/// visibility toggles (absent means visible).
async fn get_sources(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_READ)?;
        let mut sources: Vec<CalendarSource> = state
            .sources
            .iter()
            .filter(|entry| {
                entry.value().tenant_id == caller.tenant_id
                    && entry.value().user_id == caller.actor
            })
            .map(|entry| entry.value().clone())
            .collect();
        sources.sort_by(|left, right| left.module.cmp(&right.module));
        Ok(json!({ "sources": sources }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// PUT /api/v1/calendar/sources — replace the caller's toggles
/// (full set; a module absent from the list is visible).
async fn update_sources(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
    body: web::Json<UpdateSourcesRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_MANAGE)?;
        let input = body.into_inner();
        if input.sources.len() > MAX_SOURCES_PER_USER {
            return Err(UnifiedError::bad_request(format!(
                "At most {MAX_SOURCES_PER_USER} source toggles may be stored"
            )));
        }
        let mut cleaned: Vec<CalendarSource> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for toggle in &input.sources {
            let module = validate_module_id(&toggle.module)?;
            if !seen.insert(module.clone()) {
                return Err(UnifiedError::bad_request(format!(
                    "Duplicate source toggle for module \"{module}\""
                )));
            }
            cleaned.push(CalendarSource {
                tenant_id: caller.tenant_id.clone(),
                user_id: caller.actor.clone(),
                module,
                visible: toggle.visible,
                updated_at: Utc::now(),
            });
        }
        let prefix = format!("{}:{}", caller.tenant_id, caller.actor);
        state.sources.retain(|key, _| !key.starts_with(&prefix));
        for source in &cleaned {
            let key = format!("{}:{}:{}", source.tenant_id, source.user_id, source.module);
            state.sources.insert(key, source.clone());
        }
        state.emit(
            "calendar.sources.updated",
            json!({
                "tenant_id": caller.tenant_id,
                "user_id": caller.actor,
                "source_count": cleaned.len(),
            }),
        );
        Ok(json!({ "sources": cleaned }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// POST /api/v1/calendar/feeds — issue a revocable iCal
/// subscription token. The raw token is returned exactly once;
/// only its SHA-256 hash is stored.
async fn create_feed(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
    body: web::Json<CreateFeedRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_MANAGE)?;
        let input = body.into_inner();
        let mut modules: Vec<String> = Vec::new();
        if let Some(requested) = &input.modules {
            if requested.len() > MAX_MODULE_FILTER {
                return Err(UnifiedError::bad_request(format!(
                    "At most {MAX_MODULE_FILTER} modules may be scoped to one feed"
                )));
            }
            for module in requested {
                let module = validate_module_id(module)?;
                if !modules.contains(&module) {
                    modules.push(module);
                }
            }
        }
        let ttl_days = input.expires_in_days.unwrap_or(DEFAULT_FEED_TTL_DAYS);
        if !(MIN_FEED_TTL_DAYS..=MAX_FEED_TTL_DAYS).contains(&ttl_days) {
            return Err(UnifiedError::bad_request(format!(
                "expires_in_days must be between {MIN_FEED_TTL_DAYS} and {MAX_FEED_TTL_DAYS}"
            )));
        }
        let label = match &input.label {
            Some(label) if label.trim().is_empty() => None,
            Some(label) if label.chars().count() > MAX_FEED_LABEL_LEN => {
                return Err(UnifiedError::bad_request(format!(
                    "label must be at most {MAX_FEED_LABEL_LEN} characters"
                )));
            }
            Some(label) => Some(label.trim().to_string()),
            None => None,
        };
        let owned = state
            .feeds
            .iter()
            .filter(|entry| {
                entry.value().tenant_id == caller.tenant_id
                    && entry.value().user_id == caller.actor
                    && !entry.value().revoked
                    && entry.value().expires_at > Utc::now()
            })
            .count();
        if owned >= MAX_FEEDS_PER_USER {
            return Err(UnifiedError::bad_request(format!(
                "At most {MAX_FEEDS_PER_USER} active feeds per user"
            )));
        }
        let feed_id = Uuid::new_v4();
        let (token, expires_at) =
            state.issue_token(feed_id, ttl_days * 86_400);
        let feed = CalendarFeed {
            id: feed_id,
            tenant_id: caller.tenant_id.clone(),
            company_id: caller.company_id.clone(),
            user_id: caller.actor.clone(),
            token_hash: CalendarState::hash_token(&token),
            modules: modules.clone(),
            label,
            expires_at,
            revoked: false,
            created_at: Utc::now(),
            last_used_at: None,
        };
        state.feeds.insert(feed_id, feed.clone());
        state.emit(
            "calendar.feed.created",
            json!({
                "tenant_id": caller.tenant_id,
                "feed_id": feed_id.to_string(),
                "user_id": caller.actor,
                "expires_at": expires_at.to_rfc3339(),
            }),
        );
        Ok(json!({
            "id": feed.id,
            "tenant_id": feed.tenant_id,
            "modules": feed.modules,
            "label": feed.label,
            "expires_at": feed.expires_at.to_rfc3339(),
            "revoked": feed.revoked,
            "created_at": feed.created_at.to_rfc3339(),
            "feed_url": format!("/api/v1/calendar/feed/{token}"),
            "token": token,
        }))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// GET /api/v1/calendar/feeds — the caller's own feed tokens
/// (metadata only; raw tokens are never returned again).
async fn list_feeds(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_MANAGE)?;
        let mut feeds: Vec<CalendarFeed> = state
            .feeds
            .iter()
            .filter(|entry| {
                entry.value().tenant_id == caller.tenant_id
                    && entry.value().user_id == caller.actor
            })
            .map(|entry| entry.value().clone())
            .collect();
        feeds.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        let page = paginate(feeds, &query);
        Ok(page.to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// DELETE /api/v1/calendar/feeds/{id} — revoke a feed token.
/// A feed from another tenant (or a missing one) answers 404.
async fn revoke_feed(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_MANAGE)?;
        let feed_id = Uuid::parse_str(path.as_str())
            .map_err(|_| UnifiedError::not_found("Feed not found"))?;
        let mut feed = state
            .feeds
            .get_mut(&feed_id)
            .filter(|entry| entry.tenant_id == caller.tenant_id)
            .ok_or_else(|| UnifiedError::not_found("Feed not found"))?;
        if feed.user_id != caller.actor {
            return Err(UnifiedError::not_found("Feed not found"));
        }
        feed.revoked = true;
        let revoked_id = feed.id;
        drop(feed);
        state.emit(
            "calendar.feed.revoked",
            json!({
                "tenant_id": caller.tenant_id,
                "feed_id": revoked_id.to_string(),
                "user_id": caller.actor,
            }),
        );
        Ok(json!({ "id": revoked_id, "revoked": true }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Verify a feed token. Every failure answers the same 404 so
/// token existence never leaks — including a token whose feed
/// belongs to another tenant. The resolved tenant must match the
/// feed's tenant.
fn verify_feed_token(
    state: &CalendarState,
    tenant_id: &str,
    raw_token: &str,
) -> Option<CalendarFeed> {
    let token = raw_token.strip_suffix(".ics").unwrap_or(raw_token);
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let feed_id = Uuid::parse_str(parts[0]).ok()?;
    let expiry: i64 = parts[1].parse().ok()?;
    if expiry <= Utc::now().timestamp() {
        return None;
    }
    let expected = state.sign_token(&feed_id.to_string(), expiry);
    if !constant_time_eq(&expected, parts[2]) {
        return None;
    }
    let mut feed = state.feeds.get_mut(&feed_id)?;
    if feed.revoked || feed.expires_at <= Utc::now() {
        return None;
    }
    if feed.tenant_id != tenant_id {
        return None;
    }
    if CalendarState::hash_token(token) != feed.token_hash {
        return None;
    }
    feed.last_used_at = Some(Utc::now());
    let verified = (*feed).clone();
    Some(verified)
}

/// GET /api/v1/calendar/feed/{token} — public iCal subscription
/// feed. No `X-User-Id` header: the signed token is the
/// credential, and the request must still resolve to the feed's
/// tenant (no tenant context, or a foreign tenant, answers 404).
/// A ".ics" suffix on the token is accepted.
async fn get_feed_ics(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match resolve_tenant(&req) {
        Ok(tenant) => tenant,
        Err(_) => {
            return err(UnifiedError::not_found("Feed not found"), debug);
        }
    };
    let feed = match verify_feed_token(&state, &tenant.tenant_id.to_string(), path.as_str()) {
        Some(feed) => feed,
        None => return err(UnifiedError::not_found("Feed not found"), debug),
    };
    let owner = Caller {
        actor: feed.user_id.clone(),
        role: String::new(),
        tenant_id: feed.tenant_id.clone(),
        schema_name: tenant.schema_name.clone(),
        company_id: feed.company_id.clone(),
    };
    let now = Utc::now();
    let from = now - chrono::Duration::days(30);
    let to = now + chrono::Duration::days(340);
    let module_filter = if feed.modules.is_empty() {
        None
    } else {
        Some(feed.modules.clone())
    };
    let (items, _) = aggregate_items(
        &ctx,
        &state,
        &owner,
        from,
        to,
        module_filter.as_deref(),
    );
    let name = feed
        .label
        .clone()
        .unwrap_or_else(|| "Unified Calendar".to_string());
    ics_response(ics_calendar(&items, &name))
}

/// Shared AI draft flow for `/ai/summarize-day` and
/// `/ai/whats-next`: build context from the caller's own
/// aggregated items for one day, emit `calendar.ai.draft_requested`,
/// and return the first hook result that carries a draft string.
/// Fails closed (503) when no AI handler answers — a summary is
/// never fabricated locally. Drafts only: the AI never creates,
/// moves, or deletes a calendar item.
fn request_ai_draft(
    ctx: &AppContext,
    caller: &Caller,
    kind: &str,
    date: NaiveDate,
    context_text: String,
    item_ids: Vec<String>,
) -> UnifiedResult<AiDraftResponse> {
    let answer = ctx.hooks.emit(
        EVENT_AI_DRAFT_REQUESTED,
        json!({
            "tenant_id": caller.tenant_id,
            "user_id": caller.actor,
            "role": caller.role,
            "kind": kind,
            "date": date.to_string(),
            "item_ids": item_ids,
            "context_text": context_text,
            "requested_at": Utc::now().to_rfc3339(),
        }),
    );
    for result in &answer.results {
        let draft = result
            .get("draft")
            .and_then(Value::as_str)
            .or_else(|| result.get("text").and_then(Value::as_str));
        if let Some(draft) = draft {
            if draft.trim().is_empty() {
                continue;
            }
            return Ok(AiDraftResponse {
                kind: kind.to_string(),
                draft: draft.to_string(),
                ai_generated: true,
                status: "draft".to_string(),
                item_ids,
                user_id: caller.actor.clone(),
                model: result
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                provider: result
                    .get("provider")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                requested_at: Utc::now(),
            });
        }
        if let Some(error) = result.get("error").and_then(Value::as_str) {
            return Err(UnifiedError::new(
                format!("AI draft request failed: {error}"),
                502,
            ));
        }
    }
    Err(UnifiedError::new(
        "AI draft service did not answer calendar.ai.draft_requested: no handler is registered for calendar drafts",
        503,
    ))
}

/// Build the grounding text for one day: one line per item.
fn day_context_text(
    ctx: &AppContext,
    state: &CalendarState,
    caller: &Caller,
    date: NaiveDate,
) -> (String, Vec<String>) {
    let day_start = Utc.from_utc_datetime(
        &date.and_hms_opt(0, 0, 0).expect("midnight is valid"),
    );
    let day_end = day_start + chrono::Duration::days(1);
    let (items, _) = aggregate_items(ctx, state, caller, day_start, day_end, None);
    let mut lines: Vec<String> = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    for item in items.iter().take(MAX_AI_ITEMS) {
        let end = item.effective_end();
        lines.push(format!(
            "- [{}] {} ({} to {}, visibility: {})",
            item.module,
            item.title,
            item.start.to_rfc3339(),
            end.to_rfc3339(),
            item.visibility,
        ));
        ids.push(format!("{}:{}", item.module, item.id));
    }
    (lines.join("\n"), ids)
}

fn parse_ai_date(raw: Option<&str>) -> UnifiedResult<NaiveDate> {
    match raw {
        Some(raw) if !raw.trim().is_empty() => NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
            .map_err(|_| {
                UnifiedError::bad_request("date must be an ISO calendar date such as \"2026-11-23\"")
            }),
        _ => Ok(Utc::now().date_naive()),
    }
}

/// POST /api/v1/calendar/ai/summarize-day — AI draft summary of
/// the caller's calendar day.
async fn ai_summarize_day(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
    body: web::Json<AiDraftRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiDraftResponse> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_READ)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let date = parse_ai_date(body.date.as_deref())?;
        let (context_text, item_ids) = day_context_text(&ctx, &state, &caller, date);
        if item_ids.is_empty() {
            return Err(UnifiedError::bad_request(
                "No calendar items on that date to summarize",
            ));
        }
        request_ai_draft(
            &ctx,
            &caller,
            "summarize_day",
            date,
            context_text,
            item_ids,
        )
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// POST /api/v1/calendar/ai/whats-next — AI draft briefing of
/// what comes next on the caller's calendar from the given date.
async fn ai_whats_next(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
    body: web::Json<AiDraftRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiDraftResponse> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_READ)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let date = parse_ai_date(body.date.as_deref())?;
        let (context_text, item_ids) = day_context_text(&ctx, &state, &caller, date);
        if item_ids.is_empty() {
            return Err(UnifiedError::bad_request(
                "No calendar items on that date to brief on",
            ));
        }
        request_ai_draft(
            &ctx,
            &caller,
            "whats_next",
            date,
            context_text,
            item_ids,
        )
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Hook listeners.
// ---------------------------------------------------------------------------

/// `calendar.feeds.prune_requested` — expire/revoke-stale-feed
/// pruning: delete expired tokens and revoked tokens older than
/// the retention window. (The scheduler kind
/// `calendar.feeds.prune` is not in the scheduler's KNOWN_KINDS
/// yet, so this listener is ready but no scheduler job calls it
/// today — see the module header.)
fn on_feeds_prune_requested(state: CalendarState, ctx: &mut HookContext) {
    let now = Utc::now();
    let retention = now - chrono::Duration::days(30);
    let mut pruned = 0usize;
    state.feeds.retain(|_, feed| {
        let stale = feed.expires_at <= now
            || (feed.revoked && feed.created_at <= retention);
        if stale {
            pruned += 1;
            false
        } else {
            true
        }
    });
    ctx.add_result(json!({ "pruned": pruned }));
    state.emit(
        "calendar.feed.pruned",
        json!({ "pruned": pruned, "at": now.to_rfc3339() }),
    );
}

/// Grounding for the AI capability runner: answer
/// `calendar.ai.<capability>_requested` with the caller's items
/// for the requested date. The runner's payload carries the
/// tenant id (`tenant_id`) and the caller (`caller` as a user id,
/// or `user_id`); when either is missing the listener answers
/// with an empty grounding instead of guessing a tenant — fail
/// closed, never aggregate across tenants.
fn on_ai_grounding_requested(
    state: CalendarState,
    ctx: &mut HookContext,
    capability: &str,
) {
    let data = ctx.data.clone();
    let tenant_id = data
        .get("tenant_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let user_id = data
        .get("caller")
        .and_then(Value::as_str)
        .or_else(|| data.get("user_id").and_then(Value::as_str))
        .map(str::to_string);
    let date = data
        .get("input")
        .and_then(|input| input.get("date"))
        .and_then(Value::as_str)
        .and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok())
        .unwrap_or_else(|| Utc::now().date_naive());
    let (tenant_id, user_id) = match (tenant_id, user_id) {
        (Some(tenant_id), Some(user_id)) => (tenant_id, user_id),
        _ => {
            ctx.add_result(json!({
                "capability": capability,
                "date": date.to_string(),
                "items": [],
                "context_text": "",
                "grounding_unavailable": "tenant context or caller missing",
            }));
            return;
        }
    };
    let bus = match state.bus() {
        Some(bus) => bus,
        None => {
            ctx.add_result(json!({
                "capability": capability,
                "date": date.to_string(),
                "items": [],
                "context_text": "",
                "grounding_unavailable": "hook bus not attached",
            }));
            return;
        }
    };
    let day_start = Utc.from_utc_datetime(
        &date.and_hms_opt(0, 0, 0).expect("midnight is valid"),
    );
    let day_end = day_start + chrono::Duration::days(1);
    let answer = bus.emit(
        EVENT_ITEMS_REQUESTED,
        json!({
            "tenant_id": tenant_id,
            "company_id": null,
            "user_id": user_id,
            "start": day_start.to_rfc3339(),
            "end": day_end.to_rfc3339(),
        }),
    );
    let mut items: Vec<Value> = Vec::new();
    for result in &answer.results {
        match result {
            Value::Array(values) => items.extend(values.clone()),
            Value::Object(map) => match map.get("items") {
                Some(Value::Array(values)) => items.extend(values.clone()),
                _ => items.push(result.clone()),
            },
            _ => {}
        }
    }
    items.retain(|item| {
        // Keep only items the subject may see: private items only
        // for their owner, and only items of this tenant.
        let visibility = item
            .get("visibility")
            .and_then(Value::as_str)
            .unwrap_or("shared");
        if visibility == "private"
            && item
                .get("owner_user_id")
                .and_then(Value::as_str)
                != Some(user_id.as_str())
        {
            return false;
        }
        match item.get("tenant_id").and_then(Value::as_str) {
            Some(item_tenant) => item_tenant == tenant_id,
            None => true,
        }
    });
    items.truncate(MAX_AI_ITEMS);
    let lines: Vec<String> = items
        .iter()
        .filter_map(|item| {
            let title = item.get("title")?.as_str()?;
            let module = item.get("module")?.as_str().unwrap_or("unknown");
            let start = item.get("start")?.as_str().unwrap_or("");
            Some(format!("- [{module}] {title} ({start})"))
        })
        .collect();
    ctx.add_result(json!({
        "capability": capability,
        "date": date.to_string(),
        "items": items,
        "context_text": lines.join("\n"),
    }));
}

// ---------------------------------------------------------------------------
// Module declaration.
// ---------------------------------------------------------------------------

pub struct CalendarModule {
    state: CalendarState,
}

impl CalendarModule {
    pub fn new() -> Self {
        Self {
            state: CalendarState::default(),
        }
    }
}

impl Default for CalendarModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for CalendarModule {
    fn id(&self) -> &'static str {
        "calendar"
    }

    fn name(&self) -> &'static str {
        "Calendar"
    }

    fn description(&self) -> &'static str {
        "Integrated calendar aggregation view: month/week/day/agenda views over dated items contributed by every module through the calendar.items_requested hook, per-user module visibility toggles, HMAC-signed revocable iCalendar subscription feeds, working hours and non-working days (configured weekends plus holidays from the holidays module), create-from-calendar hand-off to the owning module, conflict hints, and AI draft day summaries and briefings. Owns no dated records and never creates, moves, or deletes an item."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        register_ai_capabilities("calendar", CALENDAR_AI_CAPABILITIES);
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EVENT_FEEDS_PRUNE_REQUESTED,
            "calendar",
            Arc::new(move |ctx: &mut HookContext| {
                on_feeds_prune_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_AI_SUMMARIZE_DAY_REQUESTED,
            "calendar",
            Arc::new(move |ctx: &mut HookContext| {
                on_ai_grounding_requested(state.clone(), ctx, "summarize_day")
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_AI_WHATS_NEXT_REQUESTED,
            "calendar",
            Arc::new(move |ctx: &mut HookContext| {
                on_ai_grounding_requested(state.clone(), ctx, "whats_next")
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/calendar")
                .app_data(state)
                .route("", web::get().to(get_calendar_view))
                .route("/items", web::get().to(list_calendar_items))
                .route("/create-intent", web::post().to(create_intent))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/sources", web::get().to(get_sources))
                .route("/sources", web::put().to(update_sources))
                .route("/feeds", web::post().to(create_feed))
                .route("/feeds", web::get().to(list_feeds))
                .route("/feeds/{id}", web::delete().to(revoke_feed))
                .route("/feed/{token}", web::get().to(get_feed_ics))
                .route("/ai/summarize-day", web::post().to(ai_summarize_day))
                .route("/ai/whats-next", web::post().to(ai_whats_next)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/calendar", owner: "calendar", description: "Month/week/day/agenda view: aggregated items with viewer-local times, non-working days, and conflict hints." },
            RouteInfo { method: "GET", path: "/api/v1/calendar/items", owner: "calendar", description: "Paginated, filtered aggregated items (filters: from, to, modules, timezone)." },
            RouteInfo { method: "POST", path: "/api/v1/calendar/create-intent", owner: "calendar", description: "Resolve the owning module's create deep link for a date/time; the calendar never creates a record itself." },
            RouteInfo { method: "GET", path: "/api/v1/calendar/settings", owner: "calendar", description: "Effective settings: company defaults merged with the caller's overrides." },
            RouteInfo { method: "PUT", path: "/api/v1/calendar/settings", owner: "calendar", description: "Save settings (scope user or company; weekend days, working hours, default view, timezone, holiday country, create-link templates)." },
            RouteInfo { method: "GET", path: "/api/v1/calendar/sources", owner: "calendar", description: "The caller's per-module visibility toggles (absent means visible)." },
            RouteInfo { method: "PUT", path: "/api/v1/calendar/sources", owner: "calendar", description: "Replace the caller's per-module visibility toggles." },
            RouteInfo { method: "POST", path: "/api/v1/calendar/feeds", owner: "calendar", description: "Issue a revocable HMAC-signed iCalendar subscription token (raw token returned once)." },
            RouteInfo { method: "GET", path: "/api/v1/calendar/feeds", owner: "calendar", description: "List the caller's own feed tokens (metadata only; paginated)." },
            RouteInfo { method: "DELETE", path: "/api/v1/calendar/feeds/{id}", owner: "calendar", description: "Revoke a feed token (a foreign tenant's feed answers 404)." },
            RouteInfo { method: "GET", path: "/api/v1/calendar/feed/{token}", owner: "calendar", description: "Public iCalendar subscription feed; the signed token is the credential and must belong to the resolved tenant (.ics suffix accepted)." },
            RouteInfo { method: "POST", path: "/api/v1/calendar/ai/summarize-day", owner: "calendar", description: "AI draft summary of the caller's calendar day (draft only; 503 when no AI handler answers)." },
            RouteInfo { method: "POST", path: "/api/v1/calendar/ai/whats-next", owner: "calendar", description: "AI draft briefing of what comes next on the caller's calendar (draft only; 503 when no AI handler answers)." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(CalendarModule::new()));
}
