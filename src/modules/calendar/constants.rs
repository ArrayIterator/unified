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
//     the same way, and a tenant-less item is trusted only when
//     it is explicitly marked system/global (`is_system`). When no
//     module answers,
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
//     kind is a known scheduler kind (listed in the scheduler's
//     `KNOWN_KINDS`) and its dispatcher emits this event, so the
//     listener is live and the scheduler runs it.
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
// `unified_core::tenancy::TenantContext::from_http_request` (the
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
// `unified_core::tenancy::TenantContext::from_http_request`
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
// signed with a key derived from the configured app secret (see
// `CalendarState::derive_signing_key`), so the key — which never
// leaves memory — is the same on every restart and outstanding
// tokens keep verifying; only the SHA-256 hash of an issued token
// is kept in the registry.
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

//! calendar module - Event names, permissions, limits and AI capabilities.

use crate::modules::ai::AiCapability;

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
/// job kind (a known scheduler kind; its dispatcher emits this
/// event, so the listener is live).
pub const EVENT_FEEDS_PRUNE_REQUESTED: &str = "calendar.feeds.prune_requested";

/// Grounding hooks answered for the AI capability runner.
pub const EVENT_AI_SUMMARIZE_DAY_REQUESTED: &str = "calendar.ai.summarize_day_requested";

pub const EVENT_AI_WHATS_NEXT_REQUESTED: &str = "calendar.ai.whats_next_requested";

/// Holiday lookup consumed from the holidays module (may not exist
/// yet; absence is handled gracefully, never an error).
pub const EVENT_HOLIDAY_LOOKUP: &str = "holidays.is_holiday";

/// Scheduler job kind for feed pruning (a known scheduler kind;
/// its dispatcher emits `calendar.feeds.prune_requested`).
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
    (
        "appointments",
        "/appointments/book?starts_at={start}&ends_at={end}",
    ),
    (
        "events",
        "/events/new?starts_at={start}&ends_at={end}&title={title}",
    ),
    (
        "meetings",
        "/meetings/new?starts_at={start}&ends_at={end}&title={title}",
    ),
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
