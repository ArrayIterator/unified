// Events Manager — Public record, request, and view types.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_events::*;
use super::handlers_tickets::*;
use super::handlers_registrations::*;
use super::handlers_reminders::*;
use super::handlers_public::*;
use super::hooks::*;
use super::module::*;

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
    pub(crate) fn as_str(&self) -> &'static str {
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
    pub(crate) fn as_str(&self) -> &'static str {
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
    pub(crate) fn rfc5545(&self) -> &'static str {
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

pub(crate) fn default_interval() -> u32 {
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

/// One ticket type of an event. Prices are money in minor units:
/// integer minor units in Rust, major units in JSON (see
/// `serde_major`). `sales_start` / `sales_end` bound the sales
/// window; `per_order_limit` caps tickets per registration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketType {
    pub id: Uuid,
    pub event_id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Price in minor units of the event currency (0 means free).
    #[serde(with = "unified_core::money::serde_major")]
    pub price: i64,
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

pub(crate) fn default_per_order() -> i64 {
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
    pub(crate) fn as_str(&self) -> &'static str {
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
    /// Total amount in minor units of the ticket currency
    /// (price * quantity). JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub total_amount: i64,
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
    #[serde(with = "unified_core::money::serde_major")]
    pub price: i64,
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
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub price: Option<i64>,
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

pub(crate) fn default_quantity() -> i64 {
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

pub(crate) fn default_export_format() -> String {
    "json".to_string()
}
