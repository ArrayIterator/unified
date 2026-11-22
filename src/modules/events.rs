// Events Manager module — conferences, workshops, meetups, and
// other scheduled gatherings, with recurring events, ticket types,
// registrations, waitlists, attendee check-in, reminders, and
// calendar feeds.
//
// Routes live under `/api/v1/events`:
//
//   Events:
//     GET/POST / — list events / create an event.
//     GET/PATCH/DELETE /{id}.
//     POST /{id}/publish, /{id}/cancel, /{id}/complete.
//     GET  /{id}/occurrences — expanded occurrences (recurrence
//       rules applied on read, overrides applied on top).
//     POST /{id}/occurrences/{occurrence_id}/cancel,
//          /{id}/occurrences/{occurrence_id}/override.
//   Ticket types:
//     GET/POST /{id}/ticket-types.
//     GET/PATCH/DELETE /{id}/ticket-types/{ticket_type_id}.
//   Registrations:
//     POST /{id}/register — register the caller (or, with the
//       manage capability, another user).
//     GET  /{id}/registrations — paginated, filter by status.
//     GET  /registrations/{registration_id}.
//     POST /registrations/{registration_id}/cancel.
//     POST /registrations/{registration_id}/refund-request.
//     POST /registrations/{registration_id}/check-in.
//   Attendees / waitlist:
//     GET  /{id}/attendees — paginated, filter by check-in status.
//     GET  /{id}/attendees/export?format=csv|json.
//     GET  /{id}/waitlist.
//   Reminders:
//     GET/POST /{id}/reminders.
//     DELETE /{id}/reminders/{reminder_id}.
//   Calendar feeds:
//     GET  /{id}/ics — one event as an RFC 5545 .ics file.
//     GET  /calendar.ics — public calendar feed.
//     GET  /calendar.json — public JSON feed.
//   Public (no auth; see the public-access note below):
//     GET  /public — published events only.
//     GET  /public/{slug} — one published event + ticket types.
//     GET  /public/{slug}/ics.
//     GET  /public/calendar.ics, /public/calendar.json.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory event, occurrence, ticket-type, registration, attendee,
// waitlist, and reminder management; recurrence expansion on read
// plus materialised occurrence rows for registration; the
// registration state machine (free registrations confirm at once,
// paid registrations confirm on `payment.captured` and move to
// `pending_verification` on `bank_transfer.verification.created`,
// mirroring the store module); waitlist auto-promotion with a
// `notification.requested` emit; attendee check-in by code (the
// code value only — no QR image is generated here); reminder rows
// with configurable offsets, queued through `scheduler.enqueue`
// with kind `events.reminders.send`; hand-built RFC 5545 .ics
// output (no calendar crate is in Cargo.toml) and a public JSON
// feed. What is NOT built here: refunds (only a
// `events.registration.refund_requested` event is emitted for the
// billing / store side); payment collection itself (the payments,
// store, and billing modules own it — this module only reacts to
// their hook events); QR image rendering; and actual email / push
// delivery (the notifications module owns delivery — this module
// only emits `notification.requested`).
//
// Public-access gate: the API Manager module enforces public access
// through its admin-managed `ApiAccessRule` records (default deny:
// an endpoint is public only when a rule exists with
// `public_access: true`, and denied outright when a rule exists
// with `allowed: false`). There is no reusable per-request
// "is this endpoint public?" helper to call from a handler — the
// check lives in the API Manager middleware / `resolve_public`
// path — so the public routes below rely on that gate: an
// administrator must create the matching `ApiAccessRule` rows for
// `GET /api/v1/events/public/*` (and for `/calendar.ics` /
// `/calendar.json` when those are meant to be public). The routes
// are registered in `route_info()` with
// `route_version_support()` defaulting to `All`, so the API
// Manager's per-version registry picks them up. Do NOT treat the
// absence of a capability check on a public route as an open
// door: without the rule the API Manager denies the call.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `events.created`, `events.updated`, `events.deleted`,
//     `events.published`, `events.cancelled`, `events.completed`
//   - `events.occurrence.cancelled`, `events.occurrence.overridden`
//   - `events.ticket_type.created`, `events.ticket_type.updated`,
//     `events.ticket_type.deleted`
//   - `events.registration.created`, `events.registration.confirmed`,
//     `events.registration.cancelled`,
//     `events.registration.refund_requested`,
//     `events.registration.checked_in`,
//     `events.registration.pending_verification`
//   - `events.waitlist.added`, `events.waitlist.promoted`
//   - `events.reminder.created`, `events.reminder.deleted`,
//     `events.reminder.sent`
//   - `notification.requested` (reminders, confirmations,
//     waitlist promotion) with the notifications module's payload
//     shape: `{"kind", "user_id", "title", "body", "module":
//     "events", "link", "priority", "channels"}`.
//   - `scheduler.enqueue` with `{"kind": "events.reminders.send",
//     "payload": {"event_id", "reminder_id", ...}}` when a reminder
//     is created.
// Events this module listens to:
//   - `payment.captured` — confirms a paid registration whose
//     `order_id` (or `registration_id`) matches, mirroring the
//     store module's listener (status must be `captured` when
//     present; bank-transfer captures skip the amount check;
//     gateway captures must cover the registration total; only
//     `pending_payment` / `pending_verification` may move to
//     confirmed; already-confirmed is a safe no-op).
//   - `bank_transfer.verification.created` — moves a
//     `pending_payment` registration to `pending_verification`.
//   - `bank_transfer.rejected` — cancels a `pending_verification`
//     (or `pending_payment`) registration.
//   - `events.reminders.send_requested` — the event the scheduler
//     emits when an `events.reminders.send` job fires, following
//     its naming rule (`backup.run` -> `backup.run_requested`);
//     sends the due reminder through `notification.requested`.
//
// Scheduler note: `events.reminders.send` is NOT in the scheduler's
// `KNOWN_KINDS` list yet, so today the scheduler answers
// `{"queued": false, "error": "Unknown job kind: ..."}` and the
// reminder row records `scheduler_status: "declined"` instead of
// pretending it was scheduled. The reminder is still stored and
// stays usable (it can be sent manually through the send path);
// the follow-up is to add `"events.reminders.send"` to
// `KNOWN_KINDS` in `scheduler.rs` and arm the dispatch so the job
// emits `events.reminders.send_requested` with the reminder payload
// (see the integrator snippet in the module report).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_TITLE_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 5000;
const MAX_SLUG_LEN: usize = 128;
const MAX_CATEGORY_LEN: usize = 80;
const MAX_TAG_LEN: usize = 64;
const MAX_TAGS: usize = 20;
const MAX_VENUE_NAME_LEN: usize = 200;
const MAX_ADDRESS_LEN: usize = 500;
const MAX_TIMEZONE_LEN: usize = 64;
const MAX_COVER_KEY_LEN: usize = 512;
const MAX_NAME_LEN: usize = 200;
const MAX_EMAIL_LEN: usize = 254;
const MAX_CHECKIN_CODE_LEN: usize = 64;
/// Hard cap on materialised / expanded occurrences per event, so a
/// runaway recurrence rule can never allocate unbounded rows.
const MAX_OCCURRENCES: usize = 366;
/// Default reminder offsets (minutes before the event start).
const DEFAULT_REMINDER_OFFSETS_MIN: [i64; 2] = [1440, 60];
/// Maximum ticket quantity and per-order limit.
const MAX_TICKET_QUANTITY: i64 = 100_000;
const MAX_PER_ORDER: i64 = 100;

/// Capability names checked through the `role.check` hook.
const PERM_EVENTS_VIEW: &str = "events.view";
const PERM_EVENTS_CREATE: &str = "events.create";
const PERM_EVENTS_MANAGE: &str = "events.manage";
const PERM_EVENTS_CHECKIN: &str = "events.checkin";

/// Hook event names this module listens to (literals repeated here
/// so the module stays decoupled from the payments / store modules,
/// exactly like the store module repeats the payments constants).
const PAYMENT_CAPTURED: &str = "payment.captured";
const BANK_TRANSFER_VERIFICATION_CREATED: &str = "bank_transfer.verification.created";
const BANK_TRANSFER_REJECTED: &str = "bank_transfer.rejected";
const REMINDERS_SEND_REQUESTED: &str = "events.reminders.send_requested";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Lifecycle of an event: `draft` -> `published` -> `ongoing` ->
/// `completed`, with `cancelled` reachable from draft / published /
/// ongoing. `ongoing` is derived at read time when now falls inside
/// the event window; the stored status never moves to `ongoing` on
/// its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventStatus {
    Draft,
    Published,
    Ongoing,
    Completed,
    Cancelled,
}

impl Default for EventStatus {
    fn default() -> Self {
        Self::Draft
    }
}

impl EventStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Published => "published",
            Self::Ongoing => "ongoing",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Who may see the event. `draft` visibility hides it from every
/// public route even when the status is published; `private` hides
/// it from public lists but allows a direct slug lookup by an
/// authenticated caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventVisibility {
    Public,
    Draft,
    Private,
}

impl Default for EventVisibility {
    fn default() -> Self {
        Self::Public
    }
}

impl EventVisibility {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Draft => "draft",
            Self::Private => "private",
        }
    }
}

/// Recurrence frequency (RFC 5545 FREQ values).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecurrenceFrequency {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl RecurrenceFrequency {
    fn rfc5545(&self) -> &'static str {
        match self {
            Self::Daily => "DAILY",
            Self::Weekly => "WEEKLY",
            Self::Monthly => "MONTHLY",
            Self::Yearly => "YEARLY",
        }
    }
}

/// Where the event happens. `capacity` is the venue capacity; the
/// event's own capacity is the sum of its ticket-type quantities
/// (or the venue capacity when no ticket types exist). `geo` is
/// `[latitude, longitude]` when known.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Venue {
    pub name: String,
    pub address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<i64>,
}

/// Recurrence rule for a recurring event. `interval` is 1 by
/// default; `count` and `until` are mutually exclusive (when both
/// are given, `count` wins and `until` is ignored at expansion
/// time). `exceptions` are occurrence start times to skip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecurrenceRule {
    pub frequency: RecurrenceFrequency,
    #[serde(default = "default_interval")]
    pub interval: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exceptions: Vec<DateTime<Utc>>,
}

fn default_interval() -> u32 {
    1
}

/// One event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub slug: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub category: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub venue: Option<Venue>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    /// IANA timezone name the organizer scheduled in
    /// (format-checked only; stored times are UTC).
    pub timezone: String,
    pub visibility: EventVisibility,
    pub status: EventStatus,
    pub organizer_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    /// Storage module reference (target + key) for the cover image.
    /// Metadata only: no image bytes live in this module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_storage_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_storage_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<RecurrenceRule>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One materialised occurrence of an event. Recurring events get
/// one row per occurrence (up to `MAX_OCCURRENCES`); a one-off event
/// gets exactly one row mirroring its own start/end. Overrides live
/// on the row: `status` may be `cancelled` for a single occurrence,
/// and `starts_at` / `ends_at` / `venue` may differ from the series.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventOccurrence {
    pub id: Uuid,
    pub event_id: Uuid,
    /// 1-based position in the series (1 for a one-off event).
    pub sequence: u32,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    /// `scheduled`, `cancelled`, `completed`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub venue_override: Option<Venue>,
    /// True when this row differs from the series default (time or
    /// venue changed, or the occurrence was cancelled).
    pub overridden: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One ticket type of an event. Prices are money: NUMERIC(19,4) in
/// the migration, validated non-negative here, rounded to 4 decimal
/// places on write. `sales_start` / `sales_end` bound the sales
/// window; `per_order_limit` caps tickets per registration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketType {
    pub id: Uuid,
    pub event_id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Price in the event currency (0 means free).
    pub price: f64,
    pub currency: String,
    pub quantity: i64,
    pub sold: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sales_start: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sales_end: Option<DateTime<Utc>>,
    #[serde(default = "default_per_order")]
    pub per_order_limit: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn default_per_order() -> i64 {
    10
}

/// Lifecycle of a registration: `pending_payment` -> `confirmed`
/// (on `payment.captured`, or immediately when the total is 0),
/// `pending_payment` -> `pending_verification` (bank transfer),
/// `pending_verification` -> `confirmed` (capture) or `cancelled`
/// (rejection), `confirmed` -> `cancelled`, `waitlisted` while the
/// event is sold out (promoted back to `pending_payment` /
/// `confirmed` on a free seat).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationStatus {
    PendingPayment,
    PendingVerification,
    Confirmed,
    Waitlisted,
    Cancelled,
}

impl RegistrationStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::PendingPayment => "pending_payment",
            Self::PendingVerification => "pending_verification",
            Self::Confirmed => "confirmed",
            Self::Waitlisted => "waitlisted",
            Self::Cancelled => "cancelled",
        }
    }
}

/// One registration: one user, one event (optionally one
/// occurrence), one ticket type, a quantity of seats. `order_id` is
/// the store/billing order id when the registration was paid
/// through the store; `payment_reference` is the provider
/// transaction id recorded by the `payment.captured` listener.
/// `check_in_code` is the code the attendee presents at the door
/// (the code value only — QR image rendering belongs to a client
/// or a dedicated rendering service, not this module).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registration {
    pub id: Uuid,
    pub event_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurrence_id: Option<Uuid>,
    pub ticket_type_id: Uuid,
    pub user_id: String,
    pub quantity: i64,
    /// Total amount in the ticket currency (price * quantity).
    pub total_amount: f64,
    pub currency: String,
    pub status: RegistrationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment_reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment_provider: Option<String>,
    pub check_in_code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_in_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refund_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One attendee: the person behind a registration seat. A
/// registration with quantity N normally has N attendee rows (the
/// registrant plus guests); the registrant's own row is created
/// automatically, guests are added through the attendee endpoint.
/// `custom_answers` holds the answers to the event's custom
/// registration questions as a free-form JSON object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attendee {
    pub id: Uuid,
    pub event_id: Uuid,
    pub registration_id: Uuid,
    pub name: String,
    pub email: String,
    #[serde(default)]
    pub custom_answers: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_in_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One waitlist entry. `position` is 1-based in arrival order;
/// promotion takes the lowest position first.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitlistEntry {
    pub id: Uuid,
    pub event_id: Uuid,
    pub ticket_type_id: Uuid,
    pub user_id: String,
    pub quantity: i64,
    pub position: i64,
    /// `waiting`, `promoted`, `expired`, `cancelled`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One reminder for an event. `offset_minutes` is how long before
/// the event start the reminder fires (1440 = 24h, 60 = 1h by
/// default). `scheduler_status` is `not_scheduled`, `queued`, or
/// `declined` (see the scheduler note in the module header).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventReminder {
    pub id: Uuid,
    pub event_id: Uuid,
    pub offset_minutes: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurrence_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_template: Option<String>,
    /// `pending`, `sent`, `failed`, `cancelled`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler_job_id: Option<String>,
    pub scheduler_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateEventRequest {
    pub title: String,
    pub description: Option<String>,
    pub slug: Option<String>,
    #[serde(default)]
    pub category: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub venue: Option<Venue>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub timezone: Option<String>,
    pub visibility: Option<EventVisibility>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_storage_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_storage_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<RecurrenceRule>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateEventRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub slug: Option<String>,
    pub category: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    pub venue: Option<Option<Venue>>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub timezone: Option<String>,
    pub visibility: Option<EventVisibility>,
    pub company_id: Option<Option<String>>,
    pub cover_storage_target: Option<Option<String>>,
    pub cover_storage_key: Option<Option<String>>,
    pub recurrence: Option<Option<RecurrenceRule>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTicketTypeRequest {
    pub name: String,
    pub description: Option<String>,
    pub price: f64,
    pub currency: Option<String>,
    pub quantity: i64,
    pub sales_start: Option<DateTime<Utc>>,
    pub sales_end: Option<DateTime<Utc>>,
    pub per_order_limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTicketTypeRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub price: Option<f64>,
    pub currency: Option<String>,
    pub quantity: Option<i64>,
    pub sales_start: Option<Option<DateTime<Utc>>>,
    pub sales_end: Option<Option<DateTime<Utc>>>,
    pub per_order_limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    /// The user to register. Defaults to the caller. Registering
    /// another user requires the `events.manage` capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    pub ticket_type_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurrence_id: Option<Uuid>,
    #[serde(default = "default_quantity")]
    pub quantity: i64,
    /// Optional store / billing order id, when the caller already
    /// created an order for this registration through the store.
    /// The `payment.captured` listener matches on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    /// Attendee rows for the seats (first entry is the registrant
    /// when present; a registrant row is auto-created otherwise).
    #[serde(default)]
    pub attendees: Vec<AttendeeInput>,
    /// Answers to the event's custom registration questions.
    #[serde(default)]
    pub custom_answers: Value,
}

fn default_quantity() -> i64 {
    1
}

#[derive(Debug, Deserialize)]
pub struct AttendeeInput {
    pub name: String,
    pub email: String,
    #[serde(default)]
    pub custom_answers: Value,
}

#[derive(Debug, Deserialize)]
pub struct CancelRegistrationRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RefundRequestInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CheckInRequest {
    /// The check-in code printed on the ticket / QR payload. When
    /// omitted, the registration id in the path is checked in by an
    /// organizer instead (manage / check-in capability required).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OverrideOccurrenceRequest {
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub venue: Option<Venue>,
    /// When true the occurrence is cancelled instead of overridden.
    #[serde(default)]
    pub cancel: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateReminderRequest {
    pub offset_minutes: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurrence_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_template: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    #[serde(default = "default_export_format")]
    pub format: String,
}

fn default_export_format() -> String {
    "json".to_string()
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for events. The hook bus is attached in `init`
/// (the bus lives in `AppContext`); handlers tolerate a missing bus
/// by skipping event emission. Durable rows for these types are
/// defined by migration `2026-11-22-000034_events`; until that
/// migration is executed, events do not survive a restart.
#[derive(Clone)]
pub struct EventsState {
    pub events: Arc<DashMap<Uuid, Event>>,
    pub occurrences: Arc<DashMap<Uuid, EventOccurrence>>,
    pub ticket_types: Arc<DashMap<Uuid, TicketType>>,
    pub registrations: Arc<DashMap<Uuid, Registration>>,
    pub attendees: Arc<DashMap<Uuid, Attendee>>,
    pub waitlist: Arc<DashMap<Uuid, WaitlistEntry>>,
    pub reminders: Arc<DashMap<Uuid, EventReminder>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for EventsState {
    fn default() -> Self {
        Self {
            events: Arc::new(DashMap::new()),
            occurrences: Arc::new(DashMap::new()),
            ticket_types: Arc::new(DashMap::new()),
            registrations: Arc::new(DashMap::new()),
            attendees: Arc::new(DashMap::new()),
            waitlist: Arc::new(DashMap::new()),
            reminders: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl EventsState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape: `kind`, `user_id` (or `user_ids`),
    /// `title`, `body`, `module: "events"`, `link`, `priority`,
    /// `channels`. See `notifications.rs` `on_notification_requested`
    /// and `resolve_recipients`: a payload needs `user_id`,
    /// `user_ids`, or `audience`, and `title` falls back to
    /// `"Notification: {kind}"` when omitted.
    fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "events",
                "category": "events",
                "priority": "normal",
                "link": link,
            }),
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the ai module.
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
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
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the ai module's capability check).
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

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
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

/// A slug is lowercase letters, digits, and single hyphens only,
/// never leading or trailing with a hyphen, at most 128 chars.
fn valid_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > MAX_SLUG_LEN {
        return false;
    }
    if slug.starts_with('-') || slug.ends_with('-') || slug.contains("--") {
        return false;
    }
    slug.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for ch in title.to_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("event");
    }
    out.truncate(MAX_SLUG_LEN);
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// A timezone is an IANA name (`"Asia/Jakarta"`, `"UTC"`): letters,
/// digits, `_`, `-`, `+`, and `/` only, no empty segments. Shape
/// only; no conversion is done (all stored times are UTC).
fn valid_timezone(timezone: &str) -> bool {
    if timezone.is_empty() || timezone.len() > MAX_TIMEZONE_LEN {
        return false;
    }
    if timezone == "UTC" {
        return true;
    }
    timezone.contains('/')
        && timezone.split('/').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+')
        })
}

fn valid_email(email: &str) -> bool {
    if email.is_empty() || email.len() > MAX_EMAIL_LEN || email.contains(' ') {
        return false;
    }
    let mut parts = email.split('@');
    let (Some(local), Some(domain)) = (parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() || local.is_empty() || domain.is_empty() {
        return false;
    }
    domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// Money: finite, non-negative, rounded to 4 decimal places.
fn valid_price(price: f64) -> UnifiedResult<f64> {
    if !price.is_finite() || price < 0.0 {
        return Err(UnifiedError::bad_request("price must be a finite number >= 0"));
    }
    Ok((price * 10_000.0).round() / 10_000.0)
}

fn round_money(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// Currency: 3-letter ISO code, uppercased.
fn valid_currency(currency: &str) -> UnifiedResult<String> {
    let upper = currency.trim().to_uppercase();
    if upper.len() != 3 || !upper.chars().all(|c| c.is_ascii_uppercase()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code such as \"USD\"",
        ));
    }
    Ok(upper)
}

fn validate_venue(venue: &Venue) -> UnifiedResult<()> {
    if venue.name.trim().is_empty() || venue.name.len() > MAX_VENUE_NAME_LEN {
        return Err(UnifiedError::bad_request(
            "venue.name must not be empty and must be at most 200 characters",
        ));
    }
    if let Some(address) = &venue.address {
        if address.len() > MAX_ADDRESS_LEN {
            return Err(UnifiedError::bad_request(
                "venue.address must be at most 500 characters",
            ));
        }
    }
    if let Some(latitude) = venue.latitude {
        if !latitude.is_finite() || !(-90.0..=90.0).contains(&latitude) {
            return Err(UnifiedError::bad_request("venue.latitude must be between -90 and 90"));
        }
    }
    if let Some(longitude) = venue.longitude {
        if !longitude.is_finite() || !(-180.0..=180.0).contains(&longitude) {
            return Err(UnifiedError::bad_request(
                "venue.longitude must be between -180 and 180",
            ));
        }
    }
    if let Some(capacity) = venue.capacity {
        if capacity < 0 {
            return Err(UnifiedError::bad_request("venue.capacity must be >= 0"));
        }
    }
    Ok(())
}

fn validate_recurrence(rule: &RecurrenceRule, starts_at: DateTime<Utc>) -> UnifiedResult<()> {
    if rule.interval == 0 {
        return Err(UnifiedError::bad_request("recurrence.interval must be >= 1"));
    }
    if let Some(count) = rule.count {
        if count == 0 || count as usize > MAX_OCCURRENCES {
            return Err(UnifiedError::bad_request(format!(
                "recurrence.count must be between 1 and {MAX_OCCURRENCES}"
            )));
        }
    }
    if let Some(until) = rule.until {
        if until <= starts_at {
            return Err(UnifiedError::bad_request("recurrence.until must be after starts_at"));
        }
    }
    Ok(())
}

fn load_event(state: &EventsState, raw_id: &str) -> UnifiedResult<Event> {
    let id = parse_uuid(raw_id, "event")?;
    state
        .events
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Event not found: {id}")))
}

fn load_event_by_slug(state: &EventsState, slug: &str) -> UnifiedResult<Event> {
    state
        .events
        .iter()
        .map(|entry| entry.value().clone())
        .find(|event| event.slug == slug)
        .ok_or_else(|| UnifiedError::not_found(format!("Event not found: {slug}")))
}

fn load_ticket_type(state: &EventsState, event_id: Uuid, raw_id: &str) -> UnifiedResult<TicketType> {
    let id = parse_uuid(raw_id, "ticket type")?;
    state
        .ticket_types
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|ticket| ticket.event_id == event_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Ticket type not found: {id}")))
}

fn load_registration(state: &EventsState, raw_id: &str) -> UnifiedResult<Registration> {
    let id = parse_uuid(raw_id, "registration")?;
    state
        .registrations
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Registration not found: {id}")))
}

fn load_occurrence(state: &EventsState, event_id: Uuid, raw_id: &str) -> UnifiedResult<EventOccurrence> {
    let id = parse_uuid(raw_id, "occurrence")?;
    state
        .occurrences
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|occurrence| occurrence.event_id == event_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Occurrence not found: {id}")))
}

/// Effective status at read time: a published event whose window
/// contains now reads as `ongoing`; a published event whose end has
/// passed reads as `completed` (the stored status is unchanged).
fn effective_status(event: &Event, now: DateTime<Utc>) -> EventStatus {
    match event.status {
        EventStatus::Published if now >= event.starts_at && now < event.ends_at => {
            EventStatus::Ongoing
        }
        EventStatus::Published if now >= event.ends_at => EventStatus::Completed,
        other => other,
    }
}

fn public_event_view(state: &EventsState, event: &Event) -> Value {
    let now = Utc::now();
    let mut value = serde_json::to_value(event).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert(
            "status".to_string(),
            Value::String(effective_status(event, now).as_str().to_string()),
        );
        let tickets: Vec<TicketType> = state
            .ticket_types
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|ticket| ticket.event_id == event.id)
            .collect();
        map.insert(
            "ticket_types".to_string(),
            serde_json::to_value(tickets).unwrap_or(Value::Null),
        );
        map.insert(
            "available_seats".to_string(),
            Value::Number(available_seats(state, event.id).into()),
        );
    }
    value
}

/// Seats still available across all ticket types of an event
/// (quantity minus confirmed / pending seats). Never negative.
fn available_seats(state: &EventsState, event_id: Uuid) -> i64 {
    let total: i64 = state
        .ticket_types
        .iter()
        .filter(|entry| entry.value().event_id == event_id)
        .map(|entry| entry.value().quantity)
        .sum();
    let taken: i64 = state
        .registrations
        .iter()
        .filter(|entry| {
            entry.value().event_id == event_id
                && matches!(
                    entry.value().status,
                    RegistrationStatus::Confirmed
                        | RegistrationStatus::PendingPayment
                        | RegistrationStatus::PendingVerification
                )
        })
        .map(|entry| entry.value().quantity)
        .sum();
    (total - taken).max(0)
}

fn ticket_available(state: &EventsState, ticket: &TicketType, quantity: i64, now: DateTime<Utc>) -> UnifiedResult<()> {
    if ticket.sold + quantity > ticket.quantity {
        return Err(UnifiedError::conflict(format!(
            "Only {} seat(s) remain for ticket type \"{}\"",
            (ticket.quantity - ticket.sold).max(0),
            ticket.name
        )));
    }
    if let Some(start) = ticket.sales_start {
        if now < start {
            return Err(UnifiedError::bad_request("Ticket sales have not started yet"));
        }
    }
    if let Some(end) = ticket.sales_end {
        if now > end {
            return Err(UnifiedError::bad_request("Ticket sales have ended"));
        }
    }
    Ok(())
}

fn slug_in_use(state: &EventsState, slug: &str, except: Option<Uuid>) -> bool {
    state.events.iter().any(|entry| {
        entry.value().slug == slug && Some(entry.value().id) != except
    })
}

// ---------------------------------------------------------------------------
// Recurrence expansion
// ---------------------------------------------------------------------------

fn add_months(date: DateTime<Utc>, months: i32) -> DateTime<Utc> {
    let naive = date.naive_utc();
    let total_months = naive.year() * 12 + (naive.month() as i32 - 1) + months;
    let year = total_months.div_euclid(12);
    let month = total_months.rem_euclid(12) + 1;
    let day = naive.day().min(days_in_month(year, month as u32));
    let date_only = NaiveDate::from_ymd_opt(year, month as u32, day)
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(year, month as u32, 1).expect("valid month"));
    let naive_dt = date_only.and_time(naive.time());
    Utc.from_utc_datetime(&naive_dt)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

fn step_occurrence(start: DateTime<Utc>, rule: &RecurrenceRule, steps: u32) -> DateTime<Utc> {
    let interval = rule.interval.max(1) as i64;
    let steps = steps as i64;
    match rule.frequency {
        RecurrenceFrequency::Daily => start + Duration::days(interval * steps),
        RecurrenceFrequency::Weekly => start + Duration::weeks(interval * steps),
        RecurrenceFrequency::Monthly => add_months(start, (interval * steps) as i32),
        RecurrenceFrequency::Yearly => add_months(start, (interval * steps * 12) as i32),
    }
}

/// Expand a recurrence rule into occurrence start times, honouring
/// `count` (wins over `until`), `until`, and `exceptions`. The
/// series duration is the event's own end-minus-start, applied to
/// every occurrence. Capped at `MAX_OCCURRENCES`.
fn expand_recurrence(event: &Event) -> Vec<(DateTime<Utc>, DateTime<Utc>)> {
    let duration = event.ends_at - event.starts_at;
    let Some(rule) = &event.recurrence else {
        return vec![(event.starts_at, event.ends_at)];
    };
    let mut out = Vec::new();
    let mut step: u32 = 0;
    while out.len() < MAX_OCCURRENCES {
        if let Some(count) = rule.count {
            if out.len() >= count as usize {
                break;
            }
        }
        let start = step_occurrence(event.starts_at, rule, step);
        if let Some(until) = rule.until {
            if start > until {
                break;
            }
        }
        step += 1;
        if rule.exceptions.contains(&start) {
            continue;
        }
        out.push((start, start + duration));
        // A rule with neither count nor until is open-ended: stop at
        // the cap (one year of daily occurrences is the practical
        // bound for a single expansion; materialisation uses the
        // same cap).
        if rule.count.is_none() && rule.until.is_none() && out.len() >= MAX_OCCURRENCES {
            break;
        }
    }
    out
}

/// Materialise occurrence rows for an event, replacing any existing
/// rows for it. One-off events get a single row.
fn materialise_occurrences(state: &EventsState, event: &Event) {
    let stale: Vec<Uuid> = state
        .occurrences
        .iter()
        .filter(|entry| entry.value().event_id == event.id)
        .map(|entry| *entry.key())
        .collect();
    for id in stale {
        state.occurrences.remove(&id);
    }
    let now = Utc::now();
    for (index, (starts_at, ends_at)) in expand_recurrence(event).into_iter().enumerate() {
        let occurrence = EventOccurrence {
            id: Uuid::now_v7(),
            event_id: event.id,
            sequence: (index + 1) as u32,
            starts_at,
            ends_at,
            status: "scheduled".to_string(),
            venue_override: None,
            overridden: false,
            created_at: now,
            updated_at: now,
        };
        state.occurrences.insert(occurrence.id, occurrence);
    }
}

fn occurrences_for(state: &EventsState, event_id: Uuid) -> Vec<EventOccurrence> {
    let mut items: Vec<EventOccurrence> = state
        .occurrences
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|occurrence| occurrence.event_id == event_id)
        .collect();
    items.sort_by_key(|occurrence| (occurrence.starts_at, occurrence.sequence));
    items
}

// ---------------------------------------------------------------------------
// iCalendar (RFC 5545, hand-built — no calendar crate in Cargo.toml)
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

fn ics_event_block(event: &Event, occurrence: Option<&EventOccurrence>) -> String {
    let (starts_at, ends_at, location) = match occurrence {
        Some(occ) => (
            occ.starts_at,
            occ.ends_at,
            occ.venue_override
                .as_ref()
                .or(event.venue.as_ref())
                .map(|venue| venue.name.clone()),
        ),
        None => (
            event.starts_at,
            event.ends_at,
            event.venue.as_ref().map(|venue| venue.name.clone()),
        ),
    };
    let uid = match occurrence {
        Some(occ) => format!("{}-{}@events.unified", event.id, occ.id),
        None => format!("{}@events.unified", event.id),
    };
    let mut lines: Vec<String> = vec![
        "BEGIN:VEVENT".to_string(),
        format!("UID:{uid}"),
        format!("DTSTAMP:{}", ics_datetime(Utc::now())),
        format!("DTSTART:{}", ics_datetime(starts_at)),
        format!("DTEND:{}", ics_datetime(ends_at)),
        format!("SUMMARY:{}", ics_escape(&event.title)),
    ];
    if let Some(description) = &event.description {
        if !description.trim().is_empty() {
            lines.push(format!("DESCRIPTION:{}", ics_escape(description)));
        }
    }
    if let Some(name) = location {
        lines.push(format!("LOCATION:{}", ics_escape(&name)));
    }
    if !event.timezone.is_empty() {
        lines.push(format!("X-TIMEZONE:{}", ics_escape(&event.timezone)));
    }
    match event.status {
        EventStatus::Cancelled => lines.push("STATUS:CANCELLED".to_string()),
        EventStatus::Completed => lines.push("STATUS:CONFIRMED".to_string()),
        _ => lines.push("STATUS:CONFIRMED".to_string()),
    }
    if let Some(rule) = &event.recurrence {
        if occurrence.is_none() {
            let mut rrule = format!("FREQ={}", rule.frequency.rfc5545());
            if rule.interval > 1 {
                rrule.push_str(&format!(";INTERVAL={}", rule.interval));
            }
            if let Some(count) = rule.count {
                rrule.push_str(&format!(";COUNT={count}"));
            } else if let Some(until) = rule.until {
                rrule.push_str(&format!(";UNTIL={}", ics_datetime(until)));
            }
            lines.push(format!("RRULE:{rrule}"));
        }
    }
    lines.push("END:VEVENT".to_string());
    lines
        .into_iter()
        .map(|line| ics_fold(&line))
        .collect::<Vec<_>>()
        .join("\r\n")
}

fn build_ics(events: &[(Event, Option<EventOccurrence>)], calendar_name: &str) -> String {
    let mut out = String::new();
    out.push_str("BEGIN:VCALENDAR\r\n");
    out.push_str("VERSION:2.0\r\n");
    out.push_str("PRODID:-//Unified//Events Manager//EN\r\n");
    out.push_str("CALSCALE:GREGORIAN\r\n");
    out.push_str("METHOD:PUBLISH\r\n");
    out.push_str(&ics_fold(&format!("X-WR-CALNAME:{}", ics_escape(calendar_name))));
    out.push_str("\r\n");
    for (event, occurrence) in events {
        out.push_str(&ics_event_block(event, occurrence.as_ref()));
        out.push_str("\r\n");
    }
    out.push_str("END:VCALENDAR\r\n");
    out
}

// ---------------------------------------------------------------------------
// CSV export (hand-rolled: no csv crate is available, same as the
// import/export module)
// ---------------------------------------------------------------------------

fn escape_csv_field(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

fn attendees_csv(attendees: &[Attendee]) -> String {
    let mut out = String::from("id,event_id,registration_id,name,email,checked_in_at,created_at\r\n");
    for attendee in attendees {
        out.push_str(&format!(
            "{},{},{},{},{},{},{}\r\n",
            attendee.id,
            attendee.event_id,
            attendee.registration_id,
            escape_csv_field(&attendee.name),
            escape_csv_field(&attendee.email),
            attendee
                .checked_in_at
                .map(|value| value.to_rfc3339())
                .unwrap_or_default(),
            attendee.created_at.to_rfc3339(),
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Reminders (via the scheduler + notifications)
// ---------------------------------------------------------------------------

/// Queue one reminder through `scheduler.enqueue` with kind
/// `events.reminders.send`. Mirrors the meetings module's
/// `schedule_meeting`: the scheduler answers
/// `{"queued": true, "job_id": ...}` when it knows the kind, and
/// `{"queued": false, "error": "Unknown job kind: ..."}` today,
/// because `events.reminders.send` is not in `KNOWN_KINDS` yet.
/// The reminder is stored either way; `scheduler_status` records
/// the outcome honestly (`queued` / `declined`).
fn schedule_reminder(state: &EventsState, reminder: &mut EventReminder, event: &Event) {
    let Some(bus) = state.bus() else {
        reminder.scheduler_job_id = None;
        reminder.scheduler_status = "declined".to_string();
        return;
    };
    let send_at = event.starts_at - Duration::minutes(reminder.offset_minutes);
    let answer = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": "events.reminders.send",
            "payload": {
                "event_id": event.id.to_string(),
                "reminder_id": reminder.id.to_string(),
                "occurrence_id": reminder.occurrence_id.map(|id| id.to_string()),
                "send_at": send_at.to_rfc3339(),
                "offset_minutes": reminder.offset_minutes,
            },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
            reminder.scheduler_job_id = result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            reminder.scheduler_status = "queued".to_string();
        }
        _ => {
            reminder.scheduler_job_id = None;
            reminder.scheduler_status = "declined".to_string();
        }
    }
}

/// Send one reminder now: one `notification.requested` per
/// confirmed attendee of the event (or of the single occurrence
/// when the reminder names one), using the notifications module's
/// payload shape. Title/body templates support `{title}`,
/// `{starts_at}`, and `{event_id}` placeholders.
fn send_reminder(state: &EventsState, reminder: &EventReminder) -> UnifiedResult<usize> {
    let event = state
        .events
        .get(&reminder.event_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Event not found: {}", reminder.event_id)))?;
    let title_template = reminder
        .title_template
        .clone()
        .unwrap_or_else(|| "Reminder: {title}".to_string());
    let body_template = reminder.body_template.clone().unwrap_or_else(|| {
        "Your event \"{title}\" starts at {starts_at}.".to_string()
    });
    let starts_at = reminder
        .occurrence_id
        .and_then(|id| state.occurrences.get(&id).map(|entry| entry.value().starts_at))
        .unwrap_or(event.starts_at);
    let title = title_template
        .replace("{title}", &event.title)
        .replace("{starts_at}", &starts_at.to_rfc3339())
        .replace("{event_id}", &event.id.to_string());
    let body = body_template
        .replace("{title}", &event.title)
        .replace("{starts_at}", &starts_at.to_rfc3339())
        .replace("{event_id}", &event.id.to_string());
    let link = format!("/api/v1/events/public/{}", event.slug);
    let mut sent = 0usize;
    let mut seen: Vec<String> = Vec::new();
    for entry in state.registrations.iter() {
        let registration = entry.value();
        if registration.event_id != event.id
            || registration.status != RegistrationStatus::Confirmed
        {
            continue;
        }
        if let Some(occurrence_id) = reminder.occurrence_id {
            if registration.occurrence_id != Some(occurrence_id) {
                continue;
            }
        }
        if seen.contains(&registration.user_id) {
            continue;
        }
        seen.push(registration.user_id.clone());
        state.notify("event.reminder", &registration.user_id, &title, &body, Some(link.clone()));
        sent += 1;
    }
    if let Some(mut stored) = state.reminders.get_mut(&reminder.id) {
        stored.status = "sent".to_string();
        stored.sent_at = Some(Utc::now());
        stored.updated_at = Utc::now();
    }
    state.emit(
        "events.reminder.sent",
        json!({
            "event_id": event.id.to_string(),
            "reminder_id": reminder.id.to_string(),
            "recipients": sent,
        }),
    );
    Ok(sent)
}

/// Answer `events.reminders.send_requested`: the event the
/// scheduler emits when an `events.reminders.send` job fires.
/// Sends the named reminder; an unknown reminder id is ignored so
/// a stale job can never send for a deleted reminder.
fn on_reminders_send_requested(state: EventsState, ctx: &mut HookContext) {
    let reminder_id = ctx
        .data
        .get("reminder_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(reminder_id) = reminder_id else {
        ctx.add_result(json!({ "sent": false, "error": "events.reminders.send_requested requires 'reminder_id'" }));
        return;
    };
    let Some(reminder) = state
        .reminders
        .get(&reminder_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({ "sent": false, "reminder_id": reminder_id.to_string(), "error": "Reminder not found" }));
        return;
    };
    if reminder.status != "pending" {
        ctx.add_result(json!({
            "sent": false,
            "reminder_id": reminder_id.to_string(),
            "reason": format!("Reminder is {}", reminder.status),
        }));
        return;
    }
    match send_reminder(&state, &reminder) {
        Ok(sent) => ctx.add_result(json!({
            "sent": true,
            "reminder_id": reminder_id.to_string(),
            "recipients": sent,
        })),
        Err(error) => ctx.add_result(json!({
            "sent": false,
            "reminder_id": reminder_id.to_string(),
            "error": error.message,
        })),
    }
}

// ---------------------------------------------------------------------------
// Payment listeners (mirror the store module)
// ---------------------------------------------------------------------------

/// Confirm a paid registration on `payment.captured`, mirroring
/// `store.rs` `on_payment_captured`:
///
/// - The registration is matched by `registration_id` first, then
///   by `order_id` (against the registration's stored `order_id`),
///   then by `order.id` (the store module's nested shape).
/// - An explicit non-`captured` status is never treated as a
///   capture; a missing status means the event name is the claim.
/// - Only `pending_payment` / `pending_verification` may move to
///   confirmed — never `cancelled`; an already-confirmed
///   registration is a safe no-op, so redelivered captures are
///   safe.
/// - Amount check: for gateway payments the captured amount
///   (`amount_minor`, in the smallest currency unit) must cover
///   the registration total, so a partial capture never confirms
///   the whole registration. Bank-transfer captures skip the
///   check: the administrator already verified the amount when
///   approving the transfer. A currency mismatch is never applied.
/// - The payment reference (provider transaction id) is recorded
///   on the registration, and `events.registration.confirmed` is
///   emitted so other modules react exactly as they do for a free
///   confirmation.
fn on_payment_captured(state: Arc<EventsState>, ctx: &mut HookContext) {
    let registration_id = ctx
        .get("registration_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order_id")
                .and_then(|value| value.as_str())
                .and_then(|order_id| {
                    state
                        .registrations
                        .iter()
                        .find(|entry| entry.value().order_id.as_deref() == Some(order_id))
                        .map(|entry| entry.value().id.to_string())
                })
        })
        .or_else(|| {
            ctx.get("order")
                .and_then(|order| order.get("id"))
                .and_then(|id| id.as_str())
                .and_then(|order_id| {
                    state
                        .registrations
                        .iter()
                        .find(|entry| entry.value().order_id.as_deref() == Some(order_id))
                        .map(|entry| entry.value().id.to_string())
                })
        });
    let Some(registration_id) = registration_id else {
        ctx.add_result(json!({
            "events": "skipped",
            "reason": "payment.captured carries no registration id or matching order id",
        }));
        return;
    };
    if let Some(status) = ctx.get("status").and_then(|value| value.as_str()) {
        if status != "captured" {
            ctx.add_result(json!({
                "events": "skipped",
                "registration_id": registration_id,
                "reason": format!("payment.captured carries status \"{status}\", not \"captured\""),
            }));
            return;
        }
    }
    let provider = ctx
        .get("provider")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let reference = ctx
        .get("provider_transaction_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("transaction_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let amount_minor = ctx.get("amount_minor").and_then(|value| value.as_i64());
    let currency = ctx
        .get("currency")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let Ok(id) = Uuid::parse_str(&registration_id) else {
        ctx.add_result(json!({ "events": "skipped", "reason": "Invalid registration id" }));
        return;
    };
    let Some(registration) = state
        .registrations
        .get(&id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "events": "skipped",
            "registration_id": registration_id,
            "reason": "registration not found",
        }));
        return;
    };
    match registration.status {
        RegistrationStatus::Confirmed => {
            ctx.add_result(json!({
                "events": "already_confirmed",
                "registration_id": registration_id,
            }));
            return;
        }
        RegistrationStatus::Cancelled | RegistrationStatus::Waitlisted => {
            ctx.add_result(json!({
                "events": "skipped",
                "registration_id": registration_id,
                "reason": format!("registration is {} and can never move to confirmed", registration.status.as_str()),
            }));
            return;
        }
        RegistrationStatus::PendingPayment | RegistrationStatus::PendingVerification => {}
    }
    if provider.as_deref() != Some("bank_transfer") {
        if let (Some(amount), Some(event_currency)) = (amount_minor, &currency) {
            if !event_currency.eq_ignore_ascii_case(&registration.currency) {
                ctx.add_result(json!({
                    "events": "skipped",
                    "registration_id": registration_id,
                    "reason": format!(
                        "captured currency {event_currency} does not match registration currency {}",
                        registration.currency
                    ),
                }));
                return;
            }
            let total_minor = (registration.total_amount * 100.0).round() as i64;
            if amount < total_minor {
                ctx.add_result(json!({
                    "events": "skipped",
                    "registration_id": registration_id,
                    "reason": "captured amount does not cover the registration total (partial capture or split part)",
                }));
                return;
            }
        }
    }
    if let Some(mut stored) = state.registrations.get_mut(&id) {
        stored.status = RegistrationStatus::Confirmed;
        if let Some(reference) = &reference {
            stored.payment_reference = Some(reference.clone());
        }
        if let Some(provider) = &provider {
            stored.payment_provider = Some(provider.clone());
        }
        stored.updated_at = Utc::now();
    }
    if let Some(ticket) = state.ticket_types.get_mut(&registration.ticket_type_id) {
        ticket.sold += registration.quantity;
        ticket.updated_at = Utc::now();
    }
    let event_title = state
        .events
        .get(&registration.event_id)
        .map(|entry| entry.value().title.clone())
        .unwrap_or_default();
    state.emit(
        "events.registration.confirmed",
        json!({
            "event_id": registration.event_id.to_string(),
            "registration_id": registration.id.to_string(),
            "user_id": registration.user_id,
            "payment_reference": reference,
            "provider": provider,
        }),
    );
    state.notify(
        "event.registration.confirmed",
        &registration.user_id,
        &format!("Registration confirmed: {event_title}"),
        &format!(
            "Your registration for \"{event_title}\" is confirmed. Check-in code: {}.",
            registration.check_in_code
        ),
        Some(format!("/api/v1/events/{}", registration.event_id)),
    );
    ctx.add_result(json!({
        "events": "confirmed",
        "registration_id": registration_id,
        "payment_reference": reference,
    }));
}

/// A bank-transfer verification opened for a registration moves it
/// from `pending_payment` to `pending_verification`, mirroring the
/// store module's `on_bank_transfer_verification_created`.
fn on_bank_transfer_verification_created(state: Arc<EventsState>, ctx: &mut HookContext) {
    let registration_id = ctx
        .get("registration_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order_id")
                .and_then(|value| value.as_str())
                .and_then(|order_id| {
                    state
                        .registrations
                        .iter()
                        .find(|entry| entry.value().order_id.as_deref() == Some(order_id))
                        .map(|entry| entry.value().id.to_string())
                })
        });
    let Some(registration_id) = registration_id else {
        ctx.add_result(json!({
            "events": "skipped",
            "reason": "bank_transfer.verification.created carries no registration id or matching order id",
        }));
        return;
    };
    let Ok(id) = Uuid::parse_str(&registration_id) else {
        ctx.add_result(json!({ "events": "skipped", "reason": "Invalid registration id" }));
        return;
    };
    let Some(registration) = state
        .registrations
        .get(&id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "events": "skipped",
            "registration_id": registration_id,
            "reason": "registration not found",
        }));
        return;
    };
    if registration.status == RegistrationStatus::PendingVerification {
        ctx.add_result(json!({
            "events": "already_pending_verification",
            "registration_id": registration_id,
        }));
        return;
    }
    if registration.status != RegistrationStatus::PendingPayment {
        ctx.add_result(json!({
            "events": "skipped",
            "registration_id": registration_id,
            "reason": format!("registration is {} and cannot move to pending_verification", registration.status.as_str()),
        }));
        return;
    }
    if let Some(mut stored) = state.registrations.get_mut(&id) {
        stored.status = RegistrationStatus::PendingVerification;
        stored.updated_at = Utc::now();
    }
    state.emit(
        "events.registration.pending_verification",
        json!({
            "event_id": registration.event_id.to_string(),
            "registration_id": registration.id.to_string(),
            "user_id": registration.user_id,
        }),
    );
    ctx.add_result(json!({
        "events": "pending_verification",
        "registration_id": registration_id,
    }));
}

/// A rejected bank transfer cancels the registration, mirroring the
/// store module's `on_bank_transfer_rejected`. The freed seats are
/// offered to the waitlist.
fn on_bank_transfer_rejected(state: Arc<EventsState>, ctx: &mut HookContext) {
    let registration_id = ctx
        .get("registration_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order_id")
                .and_then(|value| value.as_str())
                .and_then(|order_id| {
                    state
                        .registrations
                        .iter()
                        .find(|entry| entry.value().order_id.as_deref() == Some(order_id))
                        .map(|entry| entry.value().id.to_string())
                })
        });
    let Some(registration_id) = registration_id else {
        ctx.add_result(json!({
            "events": "skipped",
            "reason": "bank_transfer.rejected carries no registration id or matching order id",
        }));
        return;
    };
    let Ok(id) = Uuid::parse_str(&registration_id) else {
        ctx.add_result(json!({ "events": "skipped", "reason": "Invalid registration id" }));
        return;
    };
    let Some(registration) = state
        .registrations
        .get(&id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "events": "skipped",
            "registration_id": registration_id,
            "reason": "registration not found",
        }));
        return;
    };
    if registration.status == RegistrationStatus::Cancelled {
        ctx.add_result(json!({
            "events": "already_cancelled",
            "registration_id": registration_id,
        }));
        return;
    }
    if !matches!(
        registration.status,
        RegistrationStatus::PendingPayment | RegistrationStatus::PendingVerification
    ) {
        ctx.add_result(json!({
            "events": "skipped",
            "registration_id": registration_id,
            "reason": format!("registration is {} and cannot be cancelled by a rejected transfer", registration.status.as_str()),
        }));
        return;
    }
    cancel_registration_inner(&state, &registration, Some("Bank transfer rejected"));
    ctx.add_result(json!({
        "events": "cancelled",
        "registration_id": registration_id,
    }));
}

// ---------------------------------------------------------------------------
// Waitlist promotion + cancellation internals
// ---------------------------------------------------------------------------

/// Cancel a registration (shared by the HTTP handler and the
/// bank-transfer rejection listener): mark it cancelled, release
/// the sold seats, remove its attendee check-ins, then promote the
/// waitlist. Returns the updated registration.
fn cancel_registration_inner(
    state: &EventsState,
    registration: &Registration,
    reason: Option<&str>,
) -> Registration {
    let now = Utc::now();
    let mut updated = registration.clone();
    updated.status = RegistrationStatus::Cancelled;
    updated.cancelled_at = Some(now);
    updated.updated_at = now;
    state.registrations.insert(updated.id, updated.clone());
    if registration.status == RegistrationStatus::Confirmed {
        if let Some(mut ticket) = state.ticket_types.get_mut(&registration.ticket_type_id) {
            ticket.sold = (ticket.sold - registration.quantity).max(0);
            ticket.updated_at = now;
        }
    }
    state.emit(
        "events.registration.cancelled",
        json!({
            "event_id": registration.event_id.to_string(),
            "registration_id": registration.id.to_string(),
            "user_id": registration.user_id,
            "reason": reason,
        }),
    );
    promote_waitlist(state, registration.event_id, registration.ticket_type_id);
    updated
}

/// Promote waitlist entries while seats are free for the ticket
/// type. Each promoted entry becomes a registration (confirmed
/// when the ticket is free, `pending_payment` otherwise) and the
/// user is notified through `notification.requested`.
fn promote_waitlist(state: &EventsState, event_id: Uuid, ticket_type_id: Uuid) {
    let Some(ticket) = state
        .ticket_types
        .get(&ticket_type_id)
        .map(|entry| entry.value().clone())
    else {
        return;
    };
    let Some(event) = state
        .events
        .get(&event_id)
        .map(|entry| entry.value().clone())
    else {
        return;
    };
    let mut waiting: Vec<WaitlistEntry> = state
        .waitlist
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| {
            entry.event_id == event_id
                && entry.ticket_type_id == ticket_type_id
                && entry.status == "waiting"
        })
        .collect();
    waiting.sort_by_key(|entry| entry.position);
    for entry in waiting {
        let mut ticket_now = match state.ticket_types.get(&ticket_type_id) {
            Some(ticket) => ticket.value().clone(),
            None => return,
        };
        if ticket_now.sold + entry.quantity > ticket_now.quantity {
            break;
        }
        let now = Utc::now();
        let total = round_money(ticket.price * entry.quantity as f64);
        let status = if total == 0.0 {
            RegistrationStatus::Confirmed
        } else {
            RegistrationStatus::PendingPayment
        };
        let registration = Registration {
            id: Uuid::now_v7(),
            event_id,
            occurrence_id: None,
            ticket_type_id,
            user_id: entry.user_id.clone(),
            quantity: entry.quantity,
            total_amount: total,
            currency: ticket.currency.clone(),
            status,
            order_id: None,
            payment_reference: None,
            payment_provider: None,
            check_in_code: Uuid::new_v4().to_string(),
            checked_in_at: None,
            cancelled_at: None,
            refund_reason: None,
            created_at: now,
            updated_at: now,
        };
        if status == RegistrationStatus::Confirmed {
            ticket_now.sold += entry.quantity;
            ticket_now.updated_at = now;
            state.ticket_types.insert(ticket_type_id, ticket_now);
        }
        state.registrations.insert(registration.id, registration.clone());
        if let Some(mut waitlist_entry) = state.waitlist.get_mut(&entry.id) {
            waitlist_entry.status = "promoted".to_string();
            waitlist_entry.updated_at = now;
        }
        state.emit(
            "events.waitlist.promoted",
            json!({
                "event_id": event_id.to_string(),
                "ticket_type_id": ticket_type_id.to_string(),
                "waitlist_id": entry.id.to_string(),
                "registration_id": registration.id.to_string(),
                "user_id": entry.user_id,
                "status": status.as_str(),
            }),
        );
        state.notify(
            "event.waitlist.promoted",
            &entry.user_id,
            &format!("A seat opened up: {}", event.title),
            &format!(
                "A seat opened up for \"{}\". Your registration is now {}.",
                event.title,
                status.as_str()
            ),
            Some(format!("/api/v1/events/public/{}", event.slug)),
        );
        let _ = ticket;
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct EventsModule {
    state: EventsState,
}

impl EventsModule {
    pub fn new() -> Self {
        Self {
            state: EventsState::default(),
        }
    }
}

impl Default for EventsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for EventsModule {
    fn id(&self) -> &'static str {
        "events"
    }

    fn name(&self) -> &'static str {
        "Events Manager"
    }

    fn description(&self) -> &'static str {
        "Events with venues, recurring series, ticket types, registrations, waitlists, attendee check-in, reminders through notifications, and iCalendar / JSON calendar feeds. Paid registrations confirm on payment.captured and move to pending_verification on bank-transfer verification, mirroring the store module."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "store", "billing", "notifications", "scheduler"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "store", "billing", "notifications", "scheduler"]
    }

    /// Sellable-product declaration (batch 2, moved here from the
    /// inherent methods: the `Module` trait now declares these, so
    /// the registry reads this override). Available as a product
    /// in the commerce category.
    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("commerce")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = Arc::new(self.state.clone());
        bus.register(
            PAYMENT_CAPTURED,
            "events",
            Arc::new(move |ctx: &mut HookContext| {
                on_payment_captured(Arc::clone(&state), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            BANK_TRANSFER_VERIFICATION_CREATED,
            "events",
            Arc::new(move |ctx: &mut HookContext| {
                on_bank_transfer_verification_created(Arc::clone(&state), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            BANK_TRANSFER_REJECTED,
            "events",
            Arc::new(move |ctx: &mut HookContext| {
                on_bank_transfer_rejected(Arc::clone(&state), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            REMINDERS_SEND_REQUESTED,
            "events",
            Arc::new(move |ctx: &mut HookContext| {
                on_reminders_send_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/events")
                .app_data(state)
                .route("", web::get().to(list_events))
                .route("", web::post().to(create_event))
                .route("/public", web::get().to(list_public_events))
                .route("/public/{slug}", web::get().to(get_public_event))
                .route("/public/{slug}/ics", web::get().to(public_event_ics))
                .route("/public/calendar.ics", web::get().to(public_calendar_ics))
                .route("/public/calendar.json", web::get().to(public_calendar_json))
                .route("/calendar.ics", web::get().to(calendar_ics))
                .route("/calendar.json", web::get().to(calendar_json))
                .route("/registrations/{registration_id}", web::get().to(get_registration))
                .route(
                    "/registrations/{registration_id}/cancel",
                    web::post().to(cancel_registration),
                )
                .route(
                    "/registrations/{registration_id}/refund-request",
                    web::post().to(request_refund),
                )
                .route(
                    "/registrations/{registration_id}/check-in",
                    web::post().to(check_in_registration),
                )
                .route("/{id}", web::get().to(get_event))
                .route("/{id}", web::patch().to(update_event))
                .route("/{id}", web::delete().to(delete_event))
                .route("/{id}/publish", web::post().to(publish_event))
                .route("/{id}/cancel", web::post().to(cancel_event))
                .route("/{id}/complete", web::post().to(complete_event))
                .route("/{id}/occurrences", web::get().to(list_occurrences))
                .route(
                    "/{id}/occurrences/{occurrence_id}/cancel",
                    web::post().to(cancel_occurrence),
                )
                .route(
                    "/{id}/occurrences/{occurrence_id}/override",
                    web::post().to(override_occurrence),
                )
                .route("/{id}/ticket-types", web::get().to(list_ticket_types))
                .route("/{id}/ticket-types", web::post().to(create_ticket_type))
                .route(
                    "/{id}/ticket-types/{ticket_type_id}",
                    web::get().to(get_ticket_type),
                )
                .route(
                    "/{id}/ticket-types/{ticket_type_id}",
                    web::patch().to(update_ticket_type),
                )
                .route(
                    "/{id}/ticket-types/{ticket_type_id}",
                    web::delete().to(delete_ticket_type),
                )
                .route("/{id}/register", web::post().to(register_for_event))
                .route("/{id}/registrations", web::get().to(list_registrations))
                .route("/{id}/attendees", web::get().to(list_attendees))
                .route("/{id}/attendees/export", web::get().to(export_attendees))
                .route("/{id}/waitlist", web::get().to(list_waitlist))
                .route("/{id}/reminders", web::get().to(list_reminders))
                .route("/{id}/reminders", web::post().to(create_reminder))
                .route(
                    "/{id}/reminders/{reminder_id}",
                    web::delete().to(delete_reminder),
                )
                .route(
                    "/{id}/reminders/{reminder_id}/send",
                    web::post().to(send_reminder_now),
                )
                .route("/{id}/ics", web::get().to(event_ics)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/events", owner: "events", description: "List events (paginated, filter by status, visibility, category, organizer_id, from, to)." },
            RouteInfo { method: "POST", path: "/api/v1/events", owner: "events", description: "Create an event (draft; default 24h/1h reminders are created)." },
            RouteInfo { method: "GET", path: "/api/v1/events/{id}", owner: "events", description: "Get one event." },
            RouteInfo { method: "PATCH", path: "/api/v1/events/{id}", owner: "events", description: "Update a draft or published event (organizer or manage capability)." },
            RouteInfo { method: "DELETE", path: "/api/v1/events/{id}", owner: "events", description: "Delete a draft or cancelled event." },
            RouteInfo { method: "POST", path: "/api/v1/events/{id}/publish", owner: "events", description: "Publish a draft event." },
            RouteInfo { method: "POST", path: "/api/v1/events/{id}/cancel", owner: "events", description: "Cancel an event." },
            RouteInfo { method: "POST", path: "/api/v1/events/{id}/complete", owner: "events", description: "Mark an event completed." },
            RouteInfo { method: "GET", path: "/api/v1/events/{id}/occurrences", owner: "events", description: "List materialised occurrences of an event (recurrence expanded)." },
            RouteInfo { method: "POST", path: "/api/v1/events/{id}/occurrences/{occurrence_id}/cancel", owner: "events", description: "Cancel one occurrence of a recurring event." },
            RouteInfo { method: "POST", path: "/api/v1/events/{id}/occurrences/{occurrence_id}/override", owner: "events", description: "Override one occurrence's time and/or venue (or cancel it)." },
            RouteInfo { method: "GET", path: "/api/v1/events/{id}/ticket-types", owner: "events", description: "List ticket types of an event." },
            RouteInfo { method: "POST", path: "/api/v1/events/{id}/ticket-types", owner: "events", description: "Create a ticket type for an event." },
            RouteInfo { method: "GET", path: "/api/v1/events/{id}/ticket-types/{ticket_type_id}", owner: "events", description: "Get one ticket type." },
            RouteInfo { method: "PATCH", path: "/api/v1/events/{id}/ticket-types/{ticket_type_id}", owner: "events", description: "Update a ticket type (not after sales started)." },
            RouteInfo { method: "DELETE", path: "/api/v1/events/{id}/ticket-types/{ticket_type_id}", owner: "events", description: "Delete a ticket type with no sales." },
            RouteInfo { method: "POST", path: "/api/v1/events/{id}/register", owner: "events", description: "Register for an event (self-registration; free confirms at once, paid waits for payment.captured; sold out joins the waitlist)." },
            RouteInfo { method: "GET", path: "/api/v1/events/{id}/registrations", owner: "events", description: "List registrations of an event (paginated, filter by status, ticket_type_id, user_id)." },
            RouteInfo { method: "GET", path: "/api/v1/events/registrations/{registration_id}", owner: "events", description: "Get one registration (owner or manage capability)." },
            RouteInfo { method: "POST", path: "/api/v1/events/registrations/{registration_id}/cancel", owner: "events", description: "Cancel a registration (owner or manage capability); freed seats promote the waitlist." },
            RouteInfo { method: "POST", path: "/api/v1/events/registrations/{registration_id}/refund-request", owner: "events", description: "Request a refund (emits events.registration.refund_requested for the billing/store side; no refund is processed here)." },
            RouteInfo { method: "POST", path: "/api/v1/events/registrations/{registration_id}/check-in", owner: "events", description: "Check in by check-in code, or by id for an organizer (check-in capability)." },
            RouteInfo { method: "GET", path: "/api/v1/events/{id}/attendees", owner: "events", description: "List attendees of an event (paginated, filter by checked_in, registration_id)." },
            RouteInfo { method: "GET", path: "/api/v1/events/{id}/attendees/export", owner: "events", description: "Export attendees as CSV or JSON (?format=csv|json)." },
            RouteInfo { method: "GET", path: "/api/v1/events/{id}/waitlist", owner: "events", description: "List waitlist entries of an event (arrival order)." },
            RouteInfo { method: "GET", path: "/api/v1/events/{id}/reminders", owner: "events", description: "List reminders of an event." },
            RouteInfo { method: "POST", path: "/api/v1/events/{id}/reminders", owner: "events", description: "Create a reminder (offset in minutes before start; queued as events.reminders.send)." },
            RouteInfo { method: "DELETE", path: "/api/v1/events/{id}/reminders/{reminder_id}", owner: "events", description: "Delete a pending reminder." },
            RouteInfo { method: "POST", path: "/api/v1/events/{id}/reminders/{reminder_id}/send", owner: "events", description: "Send a reminder now through notification.requested." },
            RouteInfo { method: "GET", path: "/api/v1/events/{id}/ics", owner: "events", description: "One event as an RFC 5545 .ics file (recurrence included)." },
            RouteInfo { method: "GET", path: "/api/v1/events/calendar.ics", owner: "events", description: "Calendar feed (.ics) of all non-cancelled events." },
            RouteInfo { method: "GET", path: "/api/v1/events/calendar.json", owner: "events", description: "Calendar feed (JSON) of all non-cancelled events." },
            RouteInfo { method: "GET", path: "/api/v1/events/public", owner: "events", description: "PUBLIC: list published, public events (no auth; gated by the API Manager ApiAccessRule for this path)." },
            RouteInfo { method: "GET", path: "/api/v1/events/public/{slug}", owner: "events", description: "PUBLIC: one published event with ticket types and available seats (no auth; gated by the API Manager ApiAccessRule)." },
            RouteInfo { method: "GET", path: "/api/v1/events/public/{slug}/ics", owner: "events", description: "PUBLIC: one published event as .ics (no auth; gated by the API Manager ApiAccessRule)." },
            RouteInfo { method: "GET", path: "/api/v1/events/public/calendar.ics", owner: "events", description: "PUBLIC: calendar feed (.ics) of published, public events (no auth; gated by the API Manager ApiAccessRule)." },
            RouteInfo { method: "GET", path: "/api/v1/events/public/calendar.json", owner: "events", description: "PUBLIC: calendar feed (JSON) of published, public events (no auth; gated by the API Manager ApiAccessRule)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(EventsModule::new()));
}

// ---------------------------------------------------------------------------
// Default reminders
// ---------------------------------------------------------------------------

/// Create the default 24h / 1h reminders for an event, unless
/// reminders already exist for it. Called on event creation.
fn create_default_reminders(state: &EventsState, event: &Event) {
    if state
        .reminders
        .iter()
        .any(|entry| entry.value().event_id == event.id)
    {
        return;
    }
    let now = Utc::now();
    for offset in DEFAULT_REMINDER_OFFSETS_MIN {
        let mut reminder = EventReminder {
            id: Uuid::now_v7(),
            event_id: event.id,
            offset_minutes: offset,
            occurrence_id: None,
            title_template: None,
            body_template: None,
            status: "pending".to_string(),
            scheduler_job_id: None,
            scheduler_status: "not_scheduled".to_string(),
            sent_at: None,
            created_at: now,
            updated_at: now,
        };
        schedule_reminder(state, &mut reminder, event);
        state.reminders.insert(reminder.id, reminder);
    }
}

fn can_manage_event(ctx: &AppContext, role: &str, event: &Event, actor: &str) -> UnifiedResult<()> {
    if event.organizer_id == actor {
        return Ok(());
    }
    require_permission(ctx, role, PERM_EVENTS_MANAGE)
}

// ---------------------------------------------------------------------------
// Event handlers
// ---------------------------------------------------------------------------

async fn list_events(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EVENTS_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let visibility_filter = params.filters.get("visibility").cloned();
        let category_filter = params.filters.get("category").cloned();
        let organizer_filter = params.filters.get("organizer_id").cloned();
        let from_filter = params
            .filters
            .get("from")
            .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
            .map(|value| value.with_timezone(&Utc));
        let to_filter = params
            .filters
            .get("to")
            .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
            .map(|value| value.with_timezone(&Utc));
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let now = Utc::now();
        let mut items: Vec<Value> = state
            .events
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|event| {
                if let Some(status) = &status_filter {
                    if effective_status(event, now).as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(visibility) = &visibility_filter {
                    if event.visibility.as_str() != visibility.as_str() {
                        return false;
                    }
                }
                if let Some(category) = &category_filter {
                    if !event.category.iter().any(|entry| entry == category) {
                        return false;
                    }
                }
                if let Some(organizer) = &organizer_filter {
                    if &event.organizer_id != organizer {
                        return false;
                    }
                }
                if let Some(from) = from_filter {
                    if event.ends_at < from {
                        return false;
                    }
                }
                if let Some(to) = to_filter {
                    if event.starts_at > to {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {}",
                        event.title,
                        event.description.clone().unwrap_or_default(),
                        event.slug
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .map(|event| public_event_view(&state, &event))
            .collect();
        let sort = params.allowed_sort(&["title", "starts_at", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a
                    .get("title")
                    .and_then(Value::as_str)
                    .cmp(&b.get("title").and_then(Value::as_str)),
                "starts_at" => a
                    .get("starts_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("starts_at").and_then(Value::as_str)),
                _ => a
                    .get("created_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("created_at").and_then(Value::as_str)),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    body: web::Json<CreateEventRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Event> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EVENTS_CREATE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let slug = match &input.slug {
            Some(raw) => {
                let slug = raw.trim().to_lowercase();
                if !valid_slug(&slug) {
                    return Err(UnifiedError::bad_request(
                        "slug must be lowercase letters, digits, and single hyphens only (no leading/trailing hyphen, max 128 characters)",
                    ));
                }
                slug
            }
            None => slugify(&title),
        };
        if slug_in_use(&state, &slug, None) {
            return Err(UnifiedError::conflict(format!("slug is already in use: {slug}")));
        }
        let mut category: Vec<String> = Vec::new();
        for raw in &input.category {
            category.push(require_text(raw, "category", MAX_CATEGORY_LEN)?);
        }
        let mut tags: Vec<String> = Vec::new();
        for raw in &input.tags {
            if tags.len() >= MAX_TAGS {
                return Err(UnifiedError::bad_request(format!(
                    "at most {MAX_TAGS} tags are allowed"
                )));
            }
            tags.push(require_text(raw, "tag", MAX_TAG_LEN)?);
        }
        let venue = match &input.venue {
            Some(venue) => {
                validate_venue(venue)?;
                Some(venue.clone())
            }
            None => None,
        };
        if input.ends_at <= input.starts_at {
            return Err(UnifiedError::bad_request("ends_at must be after starts_at"));
        }
        let timezone = match &input.timezone {
            Some(raw) => require_text(raw, "timezone", MAX_TIMEZONE_LEN)?,
            None => "UTC".to_string(),
        };
        if !valid_timezone(&timezone) {
            return Err(UnifiedError::bad_request(format!(
                "Invalid timezone: {timezone}. Use an IANA name such as \"Asia/Jakarta\" or \"UTC\""
            )));
        }
        let recurrence = match &input.recurrence {
            Some(rule) => {
                validate_recurrence(rule, input.starts_at)?;
                Some(rule.clone())
            }
            None => None,
        };
        let company_id = match &input.company_id {
            Some(raw) => Some(require_text(raw, "company_id", 128)?),
            None => None,
        };
        let cover_storage_target = match &input.cover_storage_target {
            Some(raw) => Some(require_text(raw, "cover_storage_target", MAX_COVER_KEY_LEN)?),
            None => None,
        };
        let cover_storage_key = match &input.cover_storage_key {
            Some(raw) => Some(require_text(raw, "cover_storage_key", MAX_COVER_KEY_LEN)?),
            None => None,
        };
        if cover_storage_key.is_some() != cover_storage_target.is_some() {
            return Err(UnifiedError::bad_request(
                "cover_storage_target and cover_storage_key must be given together (storage module reference)"
            ));
        }
        let now = Utc::now();
        let event = Event {
            id: Uuid::now_v7(),
            title,
            description,
            slug,
            category,
            tags,
            venue,
            starts_at: input.starts_at,
            ends_at: input.ends_at,
            timezone,
            visibility: input.visibility.unwrap_or_default(),
            status: EventStatus::Draft,
            organizer_id: caller.actor.clone(),
            company_id,
            cover_storage_target,
            cover_storage_key,
            recurrence,
            created_at: now,
            updated_at: now,
        };
        state.events.insert(event.id, event.clone());
        materialise_occurrences(&state, &event);
        create_default_reminders(&state, &event);
        state.emit(
            "events.created",
            json!({
                "event_id": event.id.to_string(),
                "slug": event.slug,
                "title": event.title,
                "organizer_id": event.organizer_id,
                "starts_at": event.starts_at,
                "ends_at": event.ends_at,
                "status": "draft",
            }),
        );
        Ok(event)
    })();
    match result {
        Ok(event) => created(event),
        Err(error) => err(error, debug),
    }
}

async fn get_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EVENTS_VIEW)?;
        let event = load_event(&state, &path.into_inner())?;
        Ok(public_event_view(&state, &event))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateEventRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Event> = (|| {
        let caller = caller(&req)?;
        let mut event = load_event(&state, &path.into_inner())?;
        can_manage_event(&ctx, &caller.role, &event, &caller.actor)?;
        if !matches!(event.status, EventStatus::Draft | EventStatus::Published) {
            return Err(UnifiedError::conflict(format!(
                "Only draft or published events can be updated (status is \"{}\")",
                event.status.as_str()
            )));
        }
        let input = body.into_inner();
        let mut times_changed = false;
        if let Some(title) = &input.title {
            event.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &input.description {
            event.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(slug) = &input.slug {
            let slug = slug.trim().to_lowercase();
            if !valid_slug(&slug) {
                return Err(UnifiedError::bad_request(
                    "slug must be lowercase letters, digits, and single hyphens only",
                ));
            }
            if slug_in_use(&state, &slug, Some(event.id)) {
                return Err(UnifiedError::conflict(format!("slug is already in use: {slug}")));
            }
            event.slug = slug;
        }
        if let Some(category) = &input.category {
            let mut checked: Vec<String> = Vec::new();
            for raw in category {
                checked.push(require_text(raw, "category", MAX_CATEGORY_LEN)?);
            }
            event.category = checked;
        }
        if let Some(tags) = &input.tags {
            if tags.len() > MAX_TAGS {
                return Err(UnifiedError::bad_request(format!(
                    "at most {MAX_TAGS} tags are allowed"
                )));
            }
            let mut checked: Vec<String> = Vec::new();
            for raw in tags {
                checked.push(require_text(raw, "tag", MAX_TAG_LEN)?);
            }
            event.tags = checked;
        }
        if let Some(venue) = &input.venue {
            match venue {
                Some(value) => {
                    validate_venue(value)?;
                    event.venue = Some(value.clone());
                }
                None => event.venue = None,
            }
        }
        if let Some(starts_at) = input.starts_at {
            event.starts_at = starts_at;
            times_changed = true;
        }
        if let Some(ends_at) = input.ends_at {
            event.ends_at = ends_at;
            times_changed = true;
        }
        if event.ends_at <= event.starts_at {
            return Err(UnifiedError::bad_request("ends_at must be after starts_at"));
        }
        if let Some(timezone) = &input.timezone {
            let timezone = require_text(timezone, "timezone", MAX_TIMEZONE_LEN)?;
            if !valid_timezone(&timezone) {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid timezone: {timezone}"
                )));
            }
            event.timezone = timezone;
        }
        if let Some(visibility) = input.visibility {
            event.visibility = visibility;
        }
        if let Some(company_id) = &input.company_id {
            event.company_id = match company_id {
                Some(raw) => Some(require_text(raw, "company_id", 128)?),
                None => None,
            };
        }
        if let Some(target) = &input.cover_storage_target {
            event.cover_storage_target = match target {
                Some(raw) => Some(require_text(raw, "cover_storage_target", MAX_COVER_KEY_LEN)?),
                None => None,
            };
        }
        if let Some(key) = &input.cover_storage_key {
            event.cover_storage_key = match key {
                Some(raw) => Some(require_text(raw, "cover_storage_key", MAX_COVER_KEY_LEN)?),
                None => None,
            };
        }
        if event.cover_storage_key.is_some() != event.cover_storage_target.is_some() {
            return Err(UnifiedError::bad_request(
                "cover_storage_target and cover_storage_key must be given together",
            ));
        }
        if let Some(recurrence) = &input.recurrence {
            match recurrence {
                Some(rule) => {
                    validate_recurrence(rule, event.starts_at)?;
                    event.recurrence = Some(rule.clone());
                }
                None => event.recurrence = None,
            }
            times_changed = true;
        }
        event.updated_at = Utc::now();
        state.events.insert(event.id, event.clone());
        if times_changed {
            materialise_occurrences(&state, &event);
        }
        state.emit(
            "events.updated",
            json!({
                "event_id": event.id.to_string(),
                "slug": event.slug,
                "status": event.status.as_str(),
            }),
        );
        Ok(event)
    })();
    match result {
        Ok(event) => ok(event),
        Err(error) => err(error, debug),
    }
}

async fn delete_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let event = load_event(&state, &path.into_inner())?;
        can_manage_event(&ctx, &caller.role, &event, &caller.actor)?;
        if !matches!(event.status, EventStatus::Draft | EventStatus::Cancelled) {
            return Err(UnifiedError::conflict(format!(
                "Only draft or cancelled events can be deleted (status is \"{}\"); cancel it first",
                event.status.as_str()
            )));
        }
        let has_registrations = state
            .registrations
            .iter()
            .any(|entry| entry.value().event_id == event.id);
        if has_registrations {
            return Err(UnifiedError::conflict(
                "An event with registrations cannot be deleted; cancel it instead",
            ));
        }
        let id = event.id;
        state.events.remove(&id);
        let stale_occurrences: Vec<Uuid> = state
            .occurrences
            .iter()
            .filter(|entry| entry.value().event_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in stale_occurrences {
            state.occurrences.remove(&key);
        }
        let stale_reminders: Vec<Uuid> = state
            .reminders
            .iter()
            .filter(|entry| entry.value().event_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in stale_reminders {
            state.reminders.remove(&key);
        }
        let stale_tickets: Vec<Uuid> = state
            .ticket_types
            .iter()
            .filter(|entry| entry.value().event_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in stale_tickets {
            state.ticket_types.remove(&key);
        }
        let stale_waitlist: Vec<Uuid> = state
            .waitlist
            .iter()
            .filter(|entry| entry.value().event_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in stale_waitlist {
            state.waitlist.remove(&key);
        }
        state.emit(
            "events.deleted",
            json!({ "event_id": id.to_string(), "slug": event.slug }),
        );
        Ok(json!({ "deleted": true, "event_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

fn transition_event(
    ctx: &AppContext,
    state: &EventsState,
    req: &HttpRequest,
    raw_id: &str,
    from: &[EventStatus],
    to: EventStatus,
    event_name: &str,
) -> UnifiedResult<Event> {
    let caller = caller(req)?;
    let mut event = load_event(state, raw_id)?;
    can_manage_event(ctx, &caller.role, &event, &caller.actor)?;
    if !from.contains(&event.status) {
        return Err(UnifiedError::conflict(format!(
            "Only {} can move to \"{}\" (status is \"{}\")",
            from.iter()
                .map(|status| status.as_str())
                .collect::<Vec<_>>()
                .join(" or "),
            to.as_str(),
            event.status.as_str()
        )));
    }
    event.status = to;
    event.updated_at = Utc::now();
    state.events.insert(event.id, event.clone());
    state.emit(
        event_name,
        json!({
            "event_id": event.id.to_string(),
            "slug": event.slug,
            "status": to.as_str(),
        }),
    );
    Ok(event)
}

async fn publish_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = transition_event(
        &ctx,
        &state,
        &req,
        &path.into_inner(),
        &[EventStatus::Draft],
        EventStatus::Published,
        "events.published",
    );
    match result {
        Ok(event) => ok(event),
        Err(error) => err(error, debug),
    }
}

async fn cancel_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = transition_event(
        &ctx,
        &state,
        &req,
        &path.into_inner(),
        &[EventStatus::Draft, EventStatus::Published, EventStatus::Ongoing],
        EventStatus::Cancelled,
        "events.cancelled",
    );
    match result {
        Ok(event) => ok(event),
        Err(error) => err(error, debug),
    }
}

async fn complete_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = transition_event(
        &ctx,
        &state,
        &req,
        &path.into_inner(),
        &[EventStatus::Published, EventStatus::Ongoing],
        EventStatus::Completed,
        "events.completed",
    );
    match result {
        Ok(event) => ok(event),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Occurrence handlers
// ---------------------------------------------------------------------------

async fn list_occurrences(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EVENTS_VIEW)?;
        let event = load_event(&state, &path.into_inner())?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items = occurrences_for(&state, event.id);
        if let Some(status) = &status_filter {
            items.retain(|occurrence| &occurrence.status == status);
        }
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn cancel_occurrence(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EventOccurrence> = (|| {
        let caller = caller(&req)?;
        let (event_id, occurrence_id) = path.into_inner();
        let event = load_event(&state, &event_id)?;
        can_manage_event(&ctx, &caller.role, &event, &caller.actor)?;
        let mut occurrence = load_occurrence(&state, event.id, &occurrence_id)?;
        if occurrence.status == "cancelled" {
            return Err(UnifiedError::conflict("This occurrence is already cancelled"));
        }
        occurrence.status = "cancelled".to_string();
        occurrence.overridden = true;
        occurrence.updated_at = Utc::now();
        state.occurrences.insert(occurrence.id, occurrence.clone());
        state.emit(
            "events.occurrence.cancelled",
            json!({
                "event_id": event.id.to_string(),
                "occurrence_id": occurrence.id.to_string(),
                "sequence": occurrence.sequence,
                "starts_at": occurrence.starts_at,
            }),
        );
        Ok(occurrence)
    })();
    match result {
        Ok(occurrence) => ok(occurrence),
        Err(error) => err(error, debug),
    }
}

async fn override_occurrence(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<OverrideOccurrenceRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EventOccurrence> = (|| {
        let caller = caller(&req)?;
        let (event_id, occurrence_id) = path.into_inner();
        let event = load_event(&state, &event_id)?;
        can_manage_event(&ctx, &caller.role, &event, &caller.actor)?;
        let mut occurrence = load_occurrence(&state, event.id, &occurrence_id)?;
        let input = body.into_inner();
        if input.cancel {
            occurrence.status = "cancelled".to_string();
            occurrence.overridden = true;
            occurrence.updated_at = Utc::now();
            state.occurrences.insert(occurrence.id, occurrence.clone());
            state.emit(
                "events.occurrence.cancelled",
                json!({
                    "event_id": event.id.to_string(),
                    "occurrence_id": occurrence.id.to_string(),
                    "sequence": occurrence.sequence,
                }),
            );
            return Ok(occurrence);
        }
        if occurrence.status == "cancelled" {
            return Err(UnifiedError::conflict(
                "A cancelled occurrence cannot be overridden; it stays cancelled",
            ));
        }
        if let Some(starts_at) = input.starts_at {
            occurrence.starts_at = starts_at;
        }
        if let Some(ends_at) = input.ends_at {
            occurrence.ends_at = ends_at;
        }
        if occurrence.ends_at <= occurrence.starts_at {
            return Err(UnifiedError::bad_request("ends_at must be after starts_at"));
        }
        if let Some(venue) = &input.venue {
            validate_venue(venue)?;
            occurrence.venue_override = Some(venue.clone());
        }
        occurrence.overridden = true;
        occurrence.updated_at = Utc::now();
        state.occurrences.insert(occurrence.id, occurrence.clone());
        state.emit(
            "events.occurrence.overridden",
            json!({
                "event_id": event.id.to_string(),
                "occurrence_id": occurrence.id.to_string(),
                "sequence": occurrence.sequence,
                "starts_at": occurrence.starts_at,
                "ends_at": occurrence.ends_at,
            }),
        );
        Ok(occurrence)
    })();
    match result {
        Ok(occurrence) => ok(occurrence),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Ticket-type handlers
// ---------------------------------------------------------------------------

async fn list_ticket_types(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EVENTS_VIEW)?;
        let event = load_event(&state, &path.into_inner())?;
        let params = query.into_inner();
        let mut items: Vec<TicketType> = state
            .ticket_types
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|ticket| ticket.event_id == event.id)
            .collect();
        items.sort_by_key(|ticket| ticket.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_ticket_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateTicketTypeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TicketType> = (|| {
        let caller = caller(&req)?;
        let event = load_event(&state, &path.into_inner())?;
        can_manage_event(&ctx, &caller.role, &event, &caller.actor)?;
        if !matches!(event.status, EventStatus::Draft | EventStatus::Published) {
            return Err(UnifiedError::conflict(format!(
                "Ticket types can only be added to draft or published events (status is \"{}\")",
                event.status.as_str()
            )));
        }
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let price = valid_price(input.price)?;
        let currency = valid_currency(input.currency.as_deref().unwrap_or("USD"))?;
        if input.quantity < 0 || input.quantity > MAX_TICKET_QUANTITY {
            return Err(UnifiedError::bad_request(format!(
                "quantity must be between 0 and {MAX_TICKET_QUANTITY}"
            )));
        }
        if let (Some(start), Some(end)) = (input.sales_start, input.sales_end) {
            if end <= start {
                return Err(UnifiedError::bad_request("sales_end must be after sales_start"));
            }
        }
        let per_order_limit = input.per_order_limit.unwrap_or(DEFAULT_PER_ORDER);
        if per_order_limit < 1 || per_order_limit > MAX_PER_ORDER {
            return Err(UnifiedError::bad_request(format!(
                "per_order_limit must be between 1 and {MAX_PER_ORDER}"
            )));
        }
        let now = Utc::now();
        let ticket = TicketType {
            id: Uuid::now_v7(),
            event_id: event.id,
            name,
            description,
            price,
            currency,
            quantity: input.quantity,
            sold: 0,
            sales_start: input.sales_start,
            sales_end: input.sales_end,
            per_order_limit,
            created_at: now,
            updated_at: now,
        };
        state.ticket_types.insert(ticket.id, ticket.clone());
        state.emit(
            "events.ticket_type.created",
            json!({
                "event_id": event.id.to_string(),
                "ticket_type_id": ticket.id.to_string(),
                "name": ticket.name,
                "price": ticket.price,
                "currency": ticket.currency,
                "quantity": ticket.quantity,
            }),
        );
        Ok(ticket)
    })();
    match result {
        Ok(ticket) => created(ticket),
        Err(error) => err(error, debug),
    }
}

async fn get_ticket_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TicketType> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EVENTS_VIEW)?;
        let (event_id, ticket_type_id) = path.into_inner();
        let event = load_event(&state, &event_id)?;
        load_ticket_type(&state, event.id, &ticket_type_id)
    })();
    match result {
        Ok(ticket) => ok(ticket),
        Err(error) => err(error, debug),
    }
}

async fn update_ticket_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateTicketTypeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TicketType> = (|| {
        let caller = caller(&req)?;
        let (event_id, ticket_type_id) = path.into_inner();
        let event = load_event(&state, &event_id)?;
        can_manage_event(&ctx, &caller.role, &event, &caller.actor)?;
        let mut ticket = load_ticket_type(&state, event.id, &ticket_type_id)?;
        let input = body.into_inner();
        if let Some(name) = &input.name {
            ticket.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            ticket.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(price) = input.price {
            if ticket.sold > 0 {
                return Err(UnifiedError::conflict(
                    "price cannot change after sales started for this ticket type",
                ));
            }
            ticket.price = valid_price(price)?;
        }
        if let Some(currency) = &input.currency {
            if ticket.sold > 0 {
                return Err(UnifiedError::conflict(
                    "currency cannot change after sales started for this ticket type",
                ));
            }
            ticket.currency = valid_currency(currency)?;
        }
        if let Some(quantity) = input.quantity {
            if quantity < 0 || quantity > MAX_TICKET_QUANTITY {
                return Err(UnifiedError::bad_request(format!(
                    "quantity must be between 0 and {MAX_TICKET_QUANTITY}"
                )));
            }
            if quantity < ticket.sold {
                return Err(UnifiedError::conflict(format!(
                    "quantity cannot drop below the {} seat(s) already sold",
                    ticket.sold
                )));
            }
            ticket.quantity = quantity;
        }
        if let Some(sales_start) = &input.sales_start {
            ticket.sales_start = sales_start.clone();
        }
        if let Some(sales_end) = &input.sales_end {
            ticket.sales_end = sales_end.clone();
        }
        if let (Some(start), Some(end)) = (ticket.sales_start, ticket.sales_end) {
            if end <= start {
                return Err(UnifiedError::bad_request("sales_end must be after sales_start"));
            }
        }
        if let Some(limit) = input.per_order_limit {
            if limit < 1 || limit > MAX_PER_ORDER {
                return Err(UnifiedError::bad_request(format!(
                    "per_order_limit must be between 1 and {MAX_PER_ORDER}"
                )));
            }
            ticket.per_order_limit = limit;
        }
        ticket.updated_at = Utc::now();
        state.ticket_types.insert(ticket.id, ticket.clone());
        state.emit(
            "events.ticket_type.updated",
            json!({
                "event_id": event.id.to_string(),
                "ticket_type_id": ticket.id.to_string(),
            }),
        );
        Ok(ticket)
    })();
    match result {
        Ok(ticket) => ok(ticket),
        Err(error) => err(error, debug),
    }
}

async fn delete_ticket_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let (event_id, ticket_type_id) = path.into_inner();
        let event = load_event(&state, &event_id)?;
        can_manage_event(&ctx, &caller.role, &event, &caller.actor)?;
        let ticket = load_ticket_type(&state, event.id, &ticket_type_id)?;
        if ticket.sold > 0 {
            return Err(UnifiedError::conflict(
                "A ticket type with sales cannot be deleted",
            ));
        }
        let has_registrations = state
            .registrations
            .iter()
            .any(|entry| entry.value().ticket_type_id == ticket.id);
        if has_registrations {
            return Err(UnifiedError::conflict(
                "A ticket type with registrations cannot be deleted",
            ));
        }
        state.ticket_types.remove(&ticket.id);
        state.emit(
            "events.ticket_type.deleted",
            json!({
                "event_id": event.id.to_string(),
                "ticket_type_id": ticket.id.to_string(),
            }),
        );
        Ok(json!({ "deleted": true, "ticket_type_id": ticket.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Registration handlers
// ---------------------------------------------------------------------------

async fn register_for_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RegisterRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let event = load_event(&state, &path.into_inner())?;
        let input = body.into_inner();
        // A user registers themselves. Registering another user
        // requires the manage capability.
        let target_user = match &input.user_id {
            Some(user_id) if user_id != &caller.actor => {
                require_permission(&ctx, &caller.role, PERM_EVENTS_MANAGE)?;
                require_text(user_id, "user_id", 128)?
            }
            Some(user_id) => require_text(user_id, "user_id", 128)?,
            None => caller.actor.clone(),
        };
        let now = Utc::now();
        if !matches!(
            effective_status(&event, now),
            EventStatus::Published | EventStatus::Ongoing
        ) {
            return Err(UnifiedError::conflict(format!(
                "Registrations are only open for published events (status is \"{}\")",
                effective_status(&event, now).as_str()
            )));
        }
        let ticket = load_ticket_type(&state, event.id, &input.ticket_type_id.to_string())?;
        if input.quantity < 1 || input.quantity > ticket.per_order_limit {
            return Err(UnifiedError::bad_request(format!(
                "quantity must be between 1 and {} for this ticket type",
                ticket.per_order_limit
            )));
        }
        let occurrence_id = match input.occurrence_id {
            Some(id) => {
                let occurrence = load_occurrence(&state, event.id, &id.to_string())?;
                if occurrence.status == "cancelled" {
                    return Err(UnifiedError::conflict("This occurrence is cancelled"));
                }
                Some(id)
            }
            None => None,
        };
        for attendee in &input.attendees {
            require_text(&attendee.name, "attendee.name", MAX_NAME_LEN)?;
            if !valid_email(&attendee.email) {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid attendee email: {}",
                    attendee.email
                )));
            }
            if !attendee.custom_answers.is_null() && !attendee.custom_answers.is_object() {
                return Err(UnifiedError::bad_request(
                    "attendee.custom_answers must be a JSON object",
                ));
            }
        }
        if !input.custom_answers.is_null() && !input.custom_answers.is_object() {
            return Err(UnifiedError::bad_request("custom_answers must be a JSON object"));
        }
        let order_id = match &input.order_id {
            Some(raw) => Some(require_text(raw, "order_id", 128)?),
            None => None,
        };
        // Sold out: join the waitlist instead of failing.
        let remaining = ticket.quantity - ticket.sold;
        if remaining < input.quantity {
            let position = state
                .waitlist
                .iter()
                .filter(|entry| {
                    entry.value().event_id == event.id
                        && entry.value().ticket_type_id == ticket.id
                        && entry.value().status == "waiting"
                })
                .count() as i64
                + 1;
            let entry = WaitlistEntry {
                id: Uuid::now_v7(),
                event_id: event.id,
                ticket_type_id: ticket.id,
                user_id: target_user.clone(),
                quantity: input.quantity,
                position,
                status: "waiting".to_string(),
                created_at: now,
                updated_at: now,
            };
            state.waitlist.insert(entry.id, entry.clone());
            state.emit(
                "events.waitlist.added",
                json!({
                    "event_id": event.id.to_string(),
                    "ticket_type_id": ticket.id.to_string(),
                    "waitlist_id": entry.id.to_string(),
                    "user_id": target_user,
                    "quantity": input.quantity,
                    "position": position,
                }),
            );
            return Ok(json!({
                "waitlisted": true,
                "waitlist": entry,
                "message": "The event is sold out for this ticket type; you have been added to the waitlist and will be promoted automatically when a seat opens up.",
            }));
        }
        ticket_available(&state, &ticket, input.quantity, now)?;
        let total = round_money(ticket.price * input.quantity as f64);
        let status = if total == 0.0 {
            RegistrationStatus::Confirmed
        } else {
            RegistrationStatus::PendingPayment
        };
        let registration = Registration {
            id: Uuid::now_v7(),
            event_id: event.id,
            occurrence_id,
            ticket_type_id: ticket.id,
            user_id: target_user.clone(),
            quantity: input.quantity,
            total_amount: total,
            currency: ticket.currency.clone(),
            status,
            order_id,
            payment_reference: None,
            payment_provider: None,
            check_in_code: Uuid::new_v4().to_string(),
            checked_in_at: None,
            cancelled_at: None,
            refund_reason: None,
            created_at: now,
            updated_at: now,
        };
        state.registrations.insert(registration.id, registration.clone());
        if status == RegistrationStatus::Confirmed {
            if let Some(mut stored) = state.ticket_types.get_mut(&ticket.id) {
                stored.sold += input.quantity;
                stored.updated_at = now;
            }
        }
        // Attendee rows: one per supplied guest, plus the
        // registrant's own row when none was supplied.
        let mut attendee_rows: Vec<Attendee> = Vec::new();
        if input.attendees.is_empty() {
            attendee_rows.push(Attendee {
                id: Uuid::now_v7(),
                event_id: event.id,
                registration_id: registration.id,
                name: target_user.clone(),
                email: String::new(),
                custom_answers: input.custom_answers.clone(),
                checked_in_at: None,
                created_at: now,
                updated_at: now,
            });
        } else {
            for attendee in &input.attendees {
                attendee_rows.push(Attendee {
                    id: Uuid::now_v7(),
                    event_id: event.id,
                    registration_id: registration.id,
                    name: attendee.name.trim().to_string(),
                    email: attendee.email.trim().to_string(),
                    custom_answers: attendee.custom_answers.clone(),
                    checked_in_at: None,
                    created_at: now,
                    updated_at: now,
                });
            }
        }
        for attendee in &attendee_rows {
            state.attendees.insert(attendee.id, attendee.clone());
        }
        state.emit(
            "events.registration.created",
            json!({
                "event_id": event.id.to_string(),
                "registration_id": registration.id.to_string(),
                "user_id": target_user,
                "ticket_type_id": ticket.id.to_string(),
                "quantity": input.quantity,
                "total_amount": total,
                "currency": ticket.currency,
                "status": status.as_str(),
                "order_id": order_id,
            }),
        );
        if status == RegistrationStatus::Confirmed {
            state.emit(
                "events.registration.confirmed",
                json!({
                    "event_id": event.id.to_string(),
                    "registration_id": registration.id.to_string(),
                    "user_id": target_user,
                }),
            );
            state.notify(
                "event.registration.confirmed",
                &target_user,
                &format!("Registration confirmed: {}", event.title),
                &format!(
                    "Your registration for \"{}\" is confirmed. Check-in code: {}.",
                    event.title, registration.check_in_code
                ),
                Some(format!("/api/v1/events/public/{}", event.slug)),
            );
        }
        Ok(json!({
            "registration": registration,
            "attendees": attendee_rows,
            "status": status.as_str(),
            "payment_required": status == RegistrationStatus::PendingPayment,
            "message": if status == RegistrationStatus::Confirmed {
                "Registration confirmed."
            } else {
                "Registration created. It will be confirmed when payment.captured arrives for this registration (or its order_id)."
            },
        }))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn list_registrations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EVENTS_VIEW)?;
        let event = load_event(&state, &path.into_inner())?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let ticket_filter = params.filters.get("ticket_type_id").cloned();
        let user_filter = params.filters.get("user_id").cloned();
        let mut items: Vec<Registration> = state
            .registrations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|registration| {
                if registration.event_id != event.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if registration.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(ticket_id) = &ticket_filter {
                    if registration.ticket_type_id.to_string() != *ticket_id {
                        return false;
                    }
                }
                if let Some(user_id) = &user_filter {
                    if &registration.user_id != user_id {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|registration| registration.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_registration(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let registration = load_registration(&state, &path.into_inner())?;
        if registration.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_EVENTS_MANAGE)?;
        }
        let attendees: Vec<Attendee> = state
            .attendees
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|attendee| attendee.registration_id == registration.id)
            .collect();
        Ok(json!({ "registration": registration, "attendees": attendees }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn cancel_registration(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CancelRegistrationRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Registration> = (|| {
        let caller = caller(&req)?;
        let registration = load_registration(&state, &path.into_inner())?;
        if registration.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_EVENTS_MANAGE)?;
        }
        if registration.status == RegistrationStatus::Cancelled {
            return Err(UnifiedError::conflict("This registration is already cancelled"));
        }
        let reason = body
            .into_inner()
            .reason
            .map(|raw| raw.trim().to_string())
            .filter(|raw| !raw.is_empty());
        Ok(cancel_registration_inner(&state, &registration, reason.as_deref()))
    })();
    match result {
        Ok(registration) => ok(registration),
        Err(error) => err(error, debug),
    }
}

async fn request_refund(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RefundRequestInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let registration = load_registration(&state, &path.into_inner())?;
        if registration.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_EVENTS_MANAGE)?;
        }
        if !matches!(
            registration.status,
            RegistrationStatus::Confirmed
                | RegistrationStatus::PendingPayment
                | RegistrationStatus::PendingVerification
        ) {
            return Err(UnifiedError::conflict(format!(
                "A refund cannot be requested for a {} registration",
                registration.status.as_str()
            )));
        }
        let reason = body
            .into_inner()
            .reason
            .map(|raw| raw.trim().to_string())
            .filter(|raw| !raw.is_empty());
        if let Some(mut stored) = state.registrations.get_mut(&registration.id) {
            stored.refund_reason = reason.clone();
            stored.updated_at = Utc::now();
        }
        // Refunds are NOT processed here: the billing / store side
        // owns them. This only asks for one.
        state.emit(
            "events.registration.refund_requested",
            json!({
                "event_id": registration.event_id.to_string(),
                "registration_id": registration.id.to_string(),
                "user_id": registration.user_id,
                "order_id": registration.order_id,
                "total_amount": registration.total_amount,
                "currency": registration.currency,
                "payment_reference": registration.payment_reference,
                "reason": reason,
            }),
        );
        Ok(json!({
            "refund_requested": true,
            "registration_id": registration.id.to_string(),
            "message": "Refund requested. The billing/store side will process it; this module does not move money.",
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn check_in_registration(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CheckInRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let raw_id = path.into_inner();
        let input = body.into_inner();
        let now = Utc::now();
        // By code: the attendee presents the code from their ticket
        // (the QR payload is this code value). By id: an organizer
        // checks the registration in directly.
        let registration = match &input.code {
            Some(code) => {
                let code = code.trim();
                if code.is_empty() || code.len() > MAX_CHECKIN_CODE_LEN {
                    return Err(UnifiedError::bad_request("Invalid check-in code"));
                }
                state
                    .registrations
                    .iter()
                    .map(|entry| entry.value().clone())
                    .find(|registration| registration.check_in_code == code)
                    .ok_or_else(|| UnifiedError::not_found("No registration matches this check-in code"))?
            }
            None => {
                require_permission(&ctx, &caller.role, PERM_EVENTS_CHECKIN)?;
                load_registration(&state, &raw_id)?
            }
        };
        if registration.status != RegistrationStatus::Confirmed {
            return Err(UnifiedError::conflict(format!(
                "Only a confirmed registration can be checked in (status is \"{}\")",
                registration.status.as_str()
            )));
        }
        if registration.checked_in_at.is_some() {
            return Err(UnifiedError::conflict(format!(
                "Already checked in at {}",
                registration
                    .checked_in_at
                    .map(|value| value.to_rfc3339())
                    .unwrap_or_default()
            )));
        }
        if let Some(mut stored) = state.registrations.get_mut(&registration.id) {
            stored.checked_in_at = Some(now);
            stored.updated_at = now;
        }
        let mut checked_in_attendees = 0usize;
        for mut entry in state.attendees.iter_mut() {
            if entry.value().registration_id == registration.id
                && entry.value().checked_in_at.is_none()
            {
                entry.value_mut().checked_in_at = Some(now);
                entry.value_mut().updated_at = now;
                checked_in_attendees += 1;
            }
        }
        state.emit(
            "events.registration.checked_in",
            json!({
                "event_id": registration.event_id.to_string(),
                "registration_id": registration.id.to_string(),
                "user_id": registration.user_id,
                "checked_in_at": now.to_rfc3339(),
                "attendees_checked_in": checked_in_attendees,
            }),
        );
        Ok(json!({
            "checked_in": true,
            "registration_id": registration.id.to_string(),
            "user_id": registration.user_id,
            "checked_in_at": now.to_rfc3339(),
            "attendees_checked_in": checked_in_attendees,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Attendee / waitlist / reminder handlers
// ---------------------------------------------------------------------------

async fn list_attendees(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EVENTS_VIEW)?;
        let event = load_event(&state, &path.into_inner())?;
        let params = query.into_inner();
        let checked_in_filter = params.filters.get("checked_in").cloned();
        let registration_filter = params.filters.get("registration_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Attendee> = state
            .attendees
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|attendee| {
                if attendee.event_id != event.id {
                    return false;
                }
                if let Some(checked_in) = &checked_in_filter {
                    let expected = match checked_in.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if attendee.checked_in_at.is_some() != expected {
                        return false;
                    }
                }
                if let Some(registration_id) = &registration_filter {
                    if attendee.registration_id.to_string() != *registration_id {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {}", attendee.name, attendee.email).to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|attendee| attendee.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn export_attendees(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ExportQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller_result = caller(&req);
    let caller = match caller_result {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_EVENTS_VIEW) {
        return err(error, debug);
    }
    let event = match load_event(&state, &path.into_inner()) {
        Ok(event) => event,
        Err(error) => return err(error, debug),
    };
    let mut items: Vec<Attendee> = state
        .attendees
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|attendee| attendee.event_id == event.id)
        .collect();
    items.sort_by_key(|attendee| attendee.created_at);
    match query.into_inner().format.to_lowercase().as_str() {
        "csv" => HttpResponse::Ok()
            .content_type("text/csv; charset=utf-8")
            .insert_header((
                "Content-Disposition",
                format!("attachment; filename=\"event-{}-attendees.csv\"", event.slug),
            ))
            .body(attendees_csv(&items)),
        "json" => ok(json!({ "event_id": event.id.to_string(), "attendees": items })),
        other => err(
            UnifiedError::bad_request(format!("format must be \"csv\" or \"json\", got \"{other}\"")),
            debug,
        ),
    }
}

async fn list_waitlist(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EVENTS_VIEW)?;
        let event = load_event(&state, &path.into_inner())?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<WaitlistEntry> = state
            .waitlist
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                if entry.event_id != event.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &entry.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|entry| entry.position);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_reminders(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EVENTS_VIEW)?;
        let event = load_event(&state, &path.into_inner())?;
        let params = query.into_inner();
        let mut items: Vec<EventReminder> = state
            .reminders
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|reminder| reminder.event_id == event.id)
            .collect();
        items.sort_by_key(|reminder| reminder.offset_minutes);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_reminder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateReminderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EventReminder> = (|| {
        let caller = caller(&req)?;
        let event = load_event(&state, &path.into_inner())?;
        can_manage_event(&ctx, &caller.role, &event, &caller.actor)?;
        let input = body.into_inner();
        if input.offset_minutes < 0 || input.offset_minutes > 60 * 24 * 365 {
            return Err(UnifiedError::bad_request(
                "offset_minutes must be between 0 and 525600 (one year)",
            ));
        }
        let occurrence_id = match input.occurrence_id {
            Some(id) => {
                load_occurrence(&state, event.id, &id.to_string())?;
                Some(id)
            }
            None => None,
        };
        let title_template = match &input.title_template {
            Some(raw) => Some(require_text(raw, "title_template", 500)?),
            None => None,
        };
        let body_template = match &input.body_template {
            Some(raw) => Some(require_text(raw, "body_template", 2000)?),
            None => None,
        };
        let now = Utc::now();
        let mut reminder = EventReminder {
            id: Uuid::now_v7(),
            event_id: event.id,
            offset_minutes: input.offset_minutes,
            occurrence_id,
            title_template,
            body_template,
            status: "pending".to_string(),
            scheduler_job_id: None,
            scheduler_status: "not_scheduled".to_string(),
            sent_at: None,
            created_at: now,
            updated_at: now,
        };
        schedule_reminder(&state, &mut reminder, &event);
        state.reminders.insert(reminder.id, reminder.clone());
        state.emit(
            "events.reminder.created",
            json!({
                "event_id": event.id.to_string(),
                "reminder_id": reminder.id.to_string(),
                "offset_minutes": reminder.offset_minutes,
                "scheduler_job_id": reminder.scheduler_job_id,
                "scheduler_status": reminder.scheduler_status,
            }),
        );
        Ok(reminder)
    })();
    match result {
        Ok(reminder) => created(reminder),
        Err(error) => err(error, debug),
    }
}

async fn delete_reminder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let (event_id, reminder_id) = path.into_inner();
        let event = load_event(&state, &event_id)?;
        can_manage_event(&ctx, &caller.role, &event, &caller.actor)?;
        let id = parse_uuid(&reminder_id, "reminder")?;
        let reminder = state
            .reminders
            .get(&id)
            .map(|entry| entry.value().clone())
            .filter(|reminder| reminder.event_id == event.id)
            .ok_or_else(|| UnifiedError::not_found(format!("Reminder not found: {id}")))?;
        if reminder.status != "pending" {
            return Err(UnifiedError::conflict(format!(
                "Only a pending reminder can be deleted (status is \"{}\")",
                reminder.status
            )));
        }
        state.reminders.remove(&id);
        state.emit(
            "events.reminder.deleted",
            json!({
                "event_id": event.id.to_string(),
                "reminder_id": id.to_string(),
            }),
        );
        Ok(json!({ "deleted": true, "reminder_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn send_reminder_now(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let (event_id, reminder_id) = path.into_inner();
        let event = load_event(&state, &event_id)?;
        can_manage_event(&ctx, &caller.role, &event, &caller.actor)?;
        let id = parse_uuid(&reminder_id, "reminder")?;
        let reminder = state
            .reminders
            .get(&id)
            .map(|entry| entry.value().clone())
            .filter(|reminder| reminder.event_id == event.id)
            .ok_or_else(|| UnifiedError::not_found(format!("Reminder not found: {id}")))?;
        if reminder.status != "pending" {
            return Err(UnifiedError::conflict(format!(
                "Only a pending reminder can be sent (status is \"{}\")",
                reminder.status
            )));
        }
        let sent = send_reminder(&state, &reminder)?;
        Ok(json!({
            "sent": true,
            "reminder_id": id.to_string(),
            "recipients": sent,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Calendar feed handlers (authenticated)
// ---------------------------------------------------------------------------

fn feed_events(state: &EventsState, public_only: bool) -> Vec<(Event, Option<EventOccurrence>)> {
    let now = Utc::now();
    let mut out: Vec<(Event, Option<EventOccurrence>)> = Vec::new();
    let mut events: Vec<Event> = state
        .events
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|event| {
            if event.status == EventStatus::Cancelled {
                return false;
            }
            if public_only {
                if event.visibility != EventVisibility::Public {
                    return false;
                }
                if !matches!(
                    effective_status(event, now),
                    EventStatus::Published | EventStatus::Ongoing | EventStatus::Completed
                ) {
                    return false;
                }
            }
            true
        })
        .collect();
    events.sort_by_key(|event| event.starts_at);
    for event in events {
        let occurrences = occurrences_for(state, event.id);
        if occurrences.is_empty() {
            out.push((event, None));
        } else {
            for occurrence in occurrences {
                if occurrence.status == "cancelled" {
                    continue;
                }
                out.push((event.clone(), Some(occurrence)));
            }
        }
    }
    out
}

fn calendar_json_payload(state: &EventsState, public_only: bool) -> Value {
    let items: Vec<Value> = feed_events(state, public_only)
        .into_iter()
        .map(|(event, occurrence)| {
            let (starts_at, ends_at, location) = match &occurrence {
                Some(occ) => (
                    occ.starts_at,
                    occ.ends_at,
                    occ.venue_override
                        .as_ref()
                        .or(event.venue.as_ref())
                        .map(|venue| venue.name.clone()),
                ),
                None => (
                    event.starts_at,
                    event.ends_at,
                    event.venue.as_ref().map(|venue| venue.name.clone()),
                ),
            };
            json!({
                "event_id": event.id.to_string(),
                "occurrence_id": occurrence.map(|occ| occ.id.to_string()),
                "title": event.title,
                "description": event.description,
                "slug": event.slug,
                "starts_at": starts_at.to_rfc3339(),
                "ends_at": ends_at.to_rfc3339(),
                "timezone": event.timezone,
                "status": event.status.as_str(),
                "visibility": event.visibility.as_str(),
                "location": location,
                "url": format!("/api/v1/events/public/{}", event.slug),
            })
        })
        .collect();
    json!({ "events": items, "count": items.len() })
}

async fn event_ics(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_EVENTS_VIEW) {
        return err(error, debug);
    }
    let event = match load_event(&state, &path.into_inner()) {
        Ok(event) => event,
        Err(error) => return err(error, debug),
    };
    let ics = build_ics(&[(event.clone(), None)], &event.title);
    HttpResponse::Ok()
        .content_type("text/calendar; charset=utf-8")
        .insert_header((
            "Content-Disposition",
            format!("attachment; filename=\"{}.ics\"", event.slug),
        ))
        .body(ics)
}

async fn calendar_ics(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_EVENTS_VIEW) {
        return err(error, debug);
    }
    let ics = build_ics(&feed_events(&state, false), "Unified Events");
    HttpResponse::Ok()
        .content_type("text/calendar; charset=utf-8")
        .insert_header(("Content-Disposition", "attachment; filename=\"events.ics\""))
        .body(ics)
}

async fn calendar_json(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_EVENTS_VIEW) {
        return err(error, debug);
    }
    ok(calendar_json_payload(&state, false))
}

// ---------------------------------------------------------------------------
// Public handlers (no auth — gated by the API Manager's ApiAccessRule
// for each path; default deny. See the module header.)
// ---------------------------------------------------------------------------

async fn list_public_events(
    state: web::Data<EventsState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let category_filter = params.filters.get("category").cloned();
    let from_filter = params
        .filters
        .get("from")
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|value| value.with_timezone(&Utc));
    let to_filter = params
        .filters
        .get("to")
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|value| value.with_timezone(&Utc));
    let search = params.search.clone().unwrap_or_default().to_lowercase();
    let now = Utc::now();
    let mut items: Vec<Value> = state
        .events
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|event| {
            if event.visibility != EventVisibility::Public {
                return false;
            }
            if !matches!(
                effective_status(event, now),
                EventStatus::Published | EventStatus::Ongoing | EventStatus::Completed
            ) {
                return false;
            }
            if let Some(category) = &category_filter {
                if !event.category.iter().any(|entry| entry == category) {
                    return false;
                }
            }
            if let Some(from) = from_filter {
                if event.ends_at < from {
                    return false;
                }
            }
            if let Some(to) = to_filter {
                if event.starts_at > to {
                    return false;
                }
            }
            if !search.is_empty() {
                let haystack = format!(
                    "{} {} {}",
                    event.title,
                    event.description.clone().unwrap_or_default(),
                    event.slug
                )
                .to_lowercase();
                if !haystack.contains(&search) {
                    return false;
                }
            }
            true
        })
        .map(|event| public_event_view(&state, &event))
        .collect();
    items.sort_by(|a, b| {
        a.get("starts_at")
            .and_then(Value::as_str)
            .cmp(&b.get("starts_at").and_then(Value::as_str))
    });
    ok(paginate(items, &params))
}

async fn get_public_event(
    state: web::Data<EventsState>,
    path: web::Path<String>,
) -> HttpResponse {
    let slug = path.into_inner();
    match load_event_by_slug(&state, &slug) {
        Ok(event) => {
            // Private and draft-visibility events never leak through
            // a public lookup: they answer 404, the same as unknown.
            if event.visibility != EventVisibility::Public {
                return err(UnifiedError::not_found(format!("Event not found: {slug}")), false);
            }
            if !matches!(
                effective_status(&event, Utc::now()),
                EventStatus::Published | EventStatus::Ongoing | EventStatus::Completed
            ) {
                return err(UnifiedError::not_found(format!("Event not found: {slug}")), false);
            }
            ok(public_event_view(&state, &event))
        }
        Err(error) => err(error, false),
    }
}

async fn public_event_ics(state: web::Data<EventsState>, path: web::Path<String>) -> HttpResponse {
    let slug = path.into_inner();
    let event = match load_event_by_slug(&state, &slug) {
        Ok(event) => event,
        Err(error) => return err(error, false),
    };
    if event.visibility != EventVisibility::Public
        || !matches!(
            effective_status(&event, Utc::now()),
            EventStatus::Published | EventStatus::Ongoing | EventStatus::Completed
        )
    {
        return err(UnifiedError::not_found(format!("Event not found: {slug}")), false);
    }
    let ics = build_ics(&[(event.clone(), None)], &event.title);
    HttpResponse::Ok()
        .content_type("text/calendar; charset=utf-8")
        .insert_header((
            "Content-Disposition",
            format!("attachment; filename=\"{}.ics\"", event.slug),
        ))
        .body(ics)
}

async fn public_calendar_ics(state: web::Data<EventsState>) -> HttpResponse {
    let ics = build_ics(&feed_events(&state, true), "Unified Events");
    HttpResponse::Ok()
        .content_type("text/calendar; charset=utf-8")
        .insert_header(("Content-Disposition", "attachment; filename=\"events.ics\""))
        .body(ics)
}

async fn public_calendar_json(state: web::Data<EventsState>) -> HttpResponse {
    ok(calendar_json_payload(&state, true))
}

// Re-exported for the integrator's tests: the scheduler job kind
// this module enqueues, and the hook event the scheduler must emit
// when that job fires.
pub const EVENTS_REMINDERS_JOB_KIND: &str = "events.reminders.send";
pub const EVENTS_REMINDERS_SEND_REQUESTED: &str = REMINDERS_SEND_REQUESTED;
