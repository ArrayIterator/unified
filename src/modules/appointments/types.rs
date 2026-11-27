// Appointments module - bookable services, staff calendars, slot
// generation, bookings with double-booking prevention, reschedule /
// cancel with a configurable cancellation policy, no-show marking,
// reminder rules, a customer-facing public booking flow, and an
// iCalendar export of a staff member's appointments.
//
// Routes live under `/api/v1/appointments`:
//
//   Services:
//     GET/POST /services - list services / create a service.
//     GET/PATCH/DELETE /services/{id}.
//     GET  /services/{id}/slots?date=YYYY-MM-DD&staff_id=&slot_minutes=
//          - bookable slots for one service on one day.
//   Staff calendars:
//     GET/POST /staff/{staff_id}/availability - working hours, breaks,
//          and days off for one staff member.
//     PATCH/DELETE /staff/{staff_id}/availability/{availability_id}.
//     GET/POST /staff/{staff_id}/services - services a staff member
//          is assigned to (assign / unassign).
//     DELETE /staff/{staff_id}/services/{service_id}.
//     GET  /staff/{staff_id}/calendar?from=&to= - appointments.
//     GET  /staff/{staff_id}/calendar.ics - iCalendar feed.
//   Appointments:
//     GET/POST / - list appointments / create a booking.
//     GET/PATCH /{id}.
//     POST /{id}/confirm, /{id}/reschedule, /{id}/cancel,
//          /{id}/complete, /{id}/no-show.
//     GET  /{id}/ics - one appointment as an RFC 5545 .ics file.
//   Reminders:
//     GET/POST /{id}/reminders - reminder rows of an appointment.
//     DELETE /{id}/reminders/{reminder_id}.
//     POST /{id}/reminders/{reminder_id}/send - send one now.
//     GET/POST /reminder-rules - configurable reminder rules.
//     DELETE /reminder-rules/{rule_id}.
//   Settings:
//     GET/PATCH /settings - module defaults (slot length, buffers,
//          cancellation policy, guest booking, rate limit, ...).
//   Public (no auth; see the public-access note below):
//     GET  /public/services - active services only.
//     GET  /public/services/{id}/slots - public slot lookup.
//     POST /public/book - guest booking (honeypot + rate limit).
//     GET  /public/bookings/{code} - look a booking up by its
//          confirmation code.
//
// HONESTY - READ THIS FIRST. What is real in this module:
// in-memory service, availability, staff-service, appointment, and
// reminder management; slot generation from working hours minus
// breaks, days off, and already-booked (buffered) time; a server-side
// double-booking check on every create and reschedule (staff overlap
// with buffers plus a per-service capacity count); reschedule and
// cancel with a configurable notice window and fee rule; no-show and
// completion; reminder rows materialised from admin-editable rules
// and queued through `scheduler.enqueue` with kind
// `appointments.reminders.send`; confirmation / reminder /
// cancellation notifications emitted as `notification.requested`
// (the notifications module owns delivery); hand-built RFC 5545
// .ics output (no calendar crate is in Cargo.toml); guest booking
// guarded by a honeypot field and an in-memory per-client rate
// limit. What is NOT built here: durable storage (the in-memory
// DashMap stores stay authoritative until migration
// `2026-11-22-000075_appointments` is executed); atomic
// double-booking prevention under concurrency - the conflict check
// runs as check-then-insert in memory, so two simultaneous requests
// for the same slot can both pass; a database exclusion constraint
// (or a serialisable transaction around the check) is an integrator
// follow-up, and the migration carries a comment saying so; actual
// email / push delivery (notifications module); payment collection
// (a paid service records its price on the appointment, but no
// `payment.captured` listener confirms bookings - the payments /
// billing modules own collection); and invoice creation itself -
// completing an appointment only emits `billing.invoice.requested`
// and `crm.activity.requested` as the integration contract, and no
// listener for either event name exists in the billing or CRM
// modules today, so the integrator must wire the consumers.
//
// Public-access gate: the API Manager module enforces public access
// through its admin-managed `ApiAccessRule` records (default deny:
// an endpoint is public only when a rule exists with
// `public_access: true`, and denied outright when a rule exists
// with `allowed: false`). There is no reusable per-request
// "is this endpoint public?" helper to call from a handler - the
// check lives in the API Manager middleware / `resolve_public`
// path - so the public routes below rely on that gate: an
// administrator must create the matching `ApiAccessRule` rows for
// `GET /api/v1/appointments/public/*` and
// `POST /api/v1/appointments/public/book`. The routes are
// registered in `route_info()` with `route_version_support()`
// defaulting to `All`, so the API Manager's per-version registry
// picks them up. Do NOT treat the absence of a capability check on
// a public route as an open door: without the rule the API Manager
// denies the call.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
// Staff members are existing users, referenced by their user id as
// a plain string: this module never creates or duplicates user
// records, and there is no user-directory lookup hook in this
// codebase to verify a staff id against, so a staff id is validated
// for shape only (non-empty) and the assignment endpoints require
// the manage capability.
//
// Events emitted on the hook bus:
//   - `appointments.service.created`, `.updated`, `.deleted`
//   - `appointments.availability.created`, `.updated`, `.deleted`
//   - `appointments.staff.assigned`, `appointments.staff.unassigned`
//   - `appointments.created`, `appointments.confirmed`,
//     `appointments.rescheduled`, `appointments.cancelled`,
//     `appointments.completed`, `appointments.no_show`
//   - `appointments.reminder.created`, `appointments.reminder.deleted`,
//     `appointments.reminder.sent`
//   - `appointments.reminder_rule.created`,
//     `appointments.reminder_rule.deleted`
//   - `appointments.settings.updated`
//   - `billing.invoice.requested` on completion (contract only: no
//     billing listener exists for it yet - see the honesty note).
//   - `crm.activity.requested` on completion (contract only: the CRM
//     module owns fulfilment).
//   - `notification.requested` (booking confirmations, reminders,
//     cancellations, no-show) with the notifications module's
//     payload shape: `{"kind", "user_id", "title", "body", "module":
//     "appointments", "link", "priority", "channels"}`. Guest
//     bookings have no user id, so the customer email address is
//     carried as both `user_id` and `email` in the payload - the
//     notifications module uses the `email` field for the email
//     channel.
//   - `scheduler.enqueue` with `{"kind":
//     "appointments.reminders.send", "payload": {"appointment_id",
//     "reminder_id", ...}}` when a reminder row is created.
// Events this module listens to:
//   - `appointments.reminders.send_requested` - the event the
//     scheduler emits when an `appointments.reminders.send` job
//     fires, following its naming rule (`backup.run` ->
//     `backup.run_requested`); sends the due reminder through
//     `notification.requested`.
//
// Scheduler note: `appointments.reminders.send` is registered in
// the scheduler's `KNOWN_KINDS` list and dispatched in
// `scheduler/service.rs`, which emits
// `appointments.reminders.send_requested` with the reminder
// payload. The reminder row records `scheduler_status: "queued"`
// when the scheduler accepts the job and `"declined"` when it
// does not; the reminder is still stored and stays usable (it can
// be sent manually through the send path).
//
// Configurability note (standing architecture rule): nothing about
// behaviour is baked into code. Slot length, buffers, the
// cancellation policy, guest booking, the rate limit, the reminder
// channels, the allowed location types, and the confirmation
// requirement all live in `AppointmentSettings`, editable through
// `GET/PATCH /settings` by an administrator; per-service overrides
// (buffers, cancellation notice and fee) live on the service row;
// reminder offsets and channels live in admin-editable reminder
// rules. The values seeded in `AppointmentsModule::new` are
// configurable defaults, clearly marked as such.

// appointments - types.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use dashmap::DashMap;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::store::*;
use super::common::*;
use super::scheduling::*;
use super::reminders::*;
use super::ics::*;
use super::validation::*;
use super::handlers_services::*;
use super::handlers_staff::*;
use super::handlers_appointments::*;
use super::handlers_reminders::*;
use super::module::*;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_EMAIL_LEN: usize = 254;

pub(crate) const MAX_PHONE_LEN: usize = 40;

pub(crate) const MAX_NOTES_LEN: usize = 5000;

pub(crate) const MAX_REASON_LEN: usize = 1000;

pub(crate) const MAX_LOCATION_LEN: usize = 200;

pub(crate) const MAX_CHANNEL_LEN: usize = 32;

pub(crate) const MAX_USER_ID_LEN: usize = 128;

/// Bounds on numeric inputs.
pub(crate) const MIN_DURATION_MINUTES: i64 = 5;

pub(crate) const MAX_DURATION_MINUTES: i64 = 1440;

pub(crate) const MIN_SLOT_MINUTES: i64 = 5;

pub(crate) const MAX_SLOT_MINUTES: i64 = 1440;

pub(crate) const MAX_BUFFER_MINUTES: i64 = 480;

pub(crate) const MAX_CAPACITY: i64 = 1000;

pub(crate) const MAX_PRICE: i64 = 100_000_000_000;

pub(crate) const MAX_OFFSET_MINUTES: i64 = 60 * 24 * 30;

pub(crate) const MAX_NOTICE_HOURS: i64 = 24 * 30;

pub(crate) const MAX_FEE_PERCENT: f64 = 100.0;

pub(crate) const MAX_RATE_LIMIT_PER_HOUR: i64 = 1000;

pub(crate) const MAX_ADVANCE_DAYS: i64 = 365;

/// Confirmation codes are 8 characters from this unambiguous set.
pub(crate) const CONFIRMATION_CODE_CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_APPOINTMENTS_VIEW: &str = "appointments.view";

pub(crate) const PERM_APPOINTMENTS_CREATE: &str = "appointments.create";

pub(crate) const PERM_APPOINTMENTS_MANAGE: &str = "appointments.manage";

/// Hook event names this module listens to.
pub(crate) const REMINDERS_SEND_REQUESTED: &str = "appointments.reminders.send_requested";

/// Notification channels this module may request. The list is a
/// validation allow-list for reminder rules and settings; the
/// notifications module owns the channels themselves.
pub(crate) const NOTIFICATION_CHANNELS: &[&str] = &["in_app", "email", "webhook"];

/// Appointment lifecycle: `pending_confirmation` -> `confirmed` ->
/// `completed`, with `cancelled` reachable from pending_confirmation
/// / confirmed, and `no_show` reachable from confirmed only (a
/// pending booking that never confirms is cancelled, never marked
/// no-show).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppointmentStatus {
    PendingConfirmation,
    Confirmed,
    Cancelled,
    Completed,
    NoShow,
}

impl AppointmentStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::PendingConfirmation => "pending_confirmation",
            Self::Confirmed => "confirmed",
            Self::Cancelled => "cancelled",
            Self::Completed => "completed",
            Self::NoShow => "no_show",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "pending_confirmation" => Some(Self::PendingConfirmation),
            "confirmed" => Some(Self::Confirmed),
            "cancelled" => Some(Self::Cancelled),
            "completed" => Some(Self::Completed),
            "no_show" => Some(Self::NoShow),
            _ => None,
        }
    }
}

/// One bookable service: a fixed-duration offering with a price, a
/// capacity (how many bookings may overlap one slot), a location
/// mode, and optional per-service overrides of the module-wide
/// buffer and cancellation defaults (an override of `None` means
/// "use the module setting").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppointmentService {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Length of one booking in minutes.
    pub duration_minutes: i64,
    /// Price per booking in minor units of `currency` (0 means
    /// free). JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub price: i64,
    pub currency: String,
    /// How many bookings may overlap the same slot (1 = one client
    /// at a time; higher for group services).
    pub capacity: i64,
    /// Whether a staff member must be assigned to each booking.
    pub requires_staff: bool,
    /// Location mode, one of the configured
    /// `allowed_location_types` (default: in_person, online, phone).
    pub location_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Free minutes kept clear before the booking (override).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffer_before_minutes: Option<i64>,
    /// Free minutes kept clear after the booking (override).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffer_after_minutes: Option<i64>,
    /// Hours of notice required for a free cancellation (override).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation_notice_hours: Option<i64>,
    /// Fee as a percentage of the price when cancelling inside the
    /// notice window (override, 0..=100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation_fee_percent: Option<f64>,
    pub is_active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One staff availability row. `kind` is `working_hours` (a weekly
/// recurring window: `weekday` 0 = Monday .. 6 = Sunday, with an
/// optional break) or `day_off` (one calendar `date`, no hours).
/// Working-hours rows may be bounded by `valid_from` / `valid_until`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaffAvailability {
    pub id: Uuid,
    /// Existing user id of the staff member (never duplicated here).
    pub staff_user_id: String,
    /// `working_hours` or `day_off`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weekday: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<NaiveDate>,
    /// Minutes from midnight (UTC) the window starts / ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_minutes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_minutes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub break_start_minutes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub break_end_minutes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One assignment of a staff member to a service they may perform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaffService {
    pub id: Uuid,
    pub staff_user_id: String,
    pub service_id: Uuid,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One booking: a customer, a service, a staff member, and a time
/// window whose length always equals the service duration (the end
/// is computed server-side, never trusted from the request).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Appointment {
    pub id: Uuid,
    pub service_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staff_user_id: Option<String>,
    /// Logged-in customer user id, when the booking was made by an
    /// authenticated caller; `None` for guest bookings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_user_id: Option<String>,
    pub customer_name: String,
    pub customer_email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_phone: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub status: AppointmentStatus,
    /// `staff` (created by staff), `customer` (logged-in booking),
    /// or `guest` (public booking without login).
    pub source: String,
    /// Snapshot of the service price at booking time, in minor
    /// units. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub price: i64,
    pub currency: String,
    /// Fee actually charged on cancellation, if any, in minor units.
    /// JSON carries major units.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "unified_core::money::serde_major::option")]
    pub cancellation_fee_charged: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Code the customer presents to look the booking up publicly.
    pub confirmation_code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One reminder for one appointment. `offset_minutes` is how long
/// before the appointment start the reminder fires. `channels` is
/// the subset of notification channels requested for this reminder.
/// `scheduler_status` is `not_scheduled`, `queued`, or `declined`
/// (see the scheduler note in the module header).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppointmentReminder {
    pub id: Uuid,
    pub appointment_id: Uuid,
    pub offset_minutes: i64,
    pub channels: Vec<String>,
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

/// One admin-editable reminder rule: every booking (or every booking
/// of one service) materialises one reminder row per active rule
/// when it is confirmed. `service_id` of `None` means "all services".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReminderRule {
    pub id: Uuid,
    pub name: String,
    pub offset_minutes: i64,
    pub channels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_id: Option<Uuid>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Module-wide configurable defaults. Seeded in
/// `AppointmentsModule::new` and editable through `GET/PATCH
/// /settings` (manage capability). Every value is a default that an
/// administrator may change; per-service overrides win over these.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppointmentSettings {
    /// Slot step in minutes when generating bookable slots.
    pub default_slot_minutes: i64,
    /// Free minutes kept clear before / after every booking.
    pub default_buffer_before_minutes: i64,
    pub default_buffer_after_minutes: i64,
    /// Hours of notice required for a free cancellation.
    pub default_cancellation_notice_hours: i64,
    /// Fee percentage charged when cancelling inside the notice
    /// window (0..=100).
    pub default_cancellation_fee_percent: f64,
    /// Whether the public booking flow accepts guests at all.
    pub allow_guest_booking: bool,
    /// Max public bookings per client per hour (rate limit).
    pub guest_booking_rate_limit_per_hour: i64,
    /// How far ahead a booking may be made, in days.
    pub guest_booking_max_advance_days: i64,
    /// Minimum lead time for a booking, in minutes.
    pub min_advance_booking_minutes: i64,
    /// Default notification channels for reminders.
    pub default_reminder_channels: Vec<String>,
    /// Location modes a service may use.
    pub allowed_location_types: Vec<String>,
    /// When true a new booking waits for staff confirmation
    /// (`pending_confirmation`); when false it confirms at once.
    pub require_confirmation: bool,
    pub updated_at: DateTime<Utc>,
}

impl Default for AppointmentSettings {
    fn default() -> Self {
        Self {
            default_slot_minutes: 30,
            default_buffer_before_minutes: 0,
            default_buffer_after_minutes: 15,
            default_cancellation_notice_hours: 24,
            default_cancellation_fee_percent: 0.0,
            allow_guest_booking: true,
            guest_booking_rate_limit_per_hour: 5,
            guest_booking_max_advance_days: 60,
            min_advance_booking_minutes: 60,
            default_reminder_channels: vec!["in_app".to_string(), "email".to_string()],
            allowed_location_types: vec![
                "in_person".to_string(),
                "online".to_string(),
                "phone".to_string(),
            ],
            require_confirmation: true,
            updated_at: Utc::now(),
        }
    }
}

/// One bookable slot returned by the slot endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookingSlot {
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub staff_user_id: String,
    pub service_id: Uuid,
}

// Request bodies

#[derive(Debug, Deserialize)]
pub struct CreateServiceRequest {
    pub name: String,
    pub description: Option<String>,
    pub duration_minutes: i64,
    #[serde(with = "unified_core::money::serde_major")]
    pub price: i64,
    pub currency: Option<String>,
    pub capacity: Option<i64>,
    pub requires_staff: Option<bool>,
    pub location_type: Option<String>,
    pub location: Option<String>,
    pub buffer_before_minutes: Option<i64>,
    pub buffer_after_minutes: Option<i64>,
    pub cancellation_notice_hours: Option<i64>,
    pub cancellation_fee_percent: Option<f64>,
    pub is_active: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateServiceRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub duration_minutes: Option<i64>,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub price: Option<i64>,
    pub currency: Option<String>,
    pub capacity: Option<i64>,
    pub requires_staff: Option<bool>,
    pub location_type: Option<String>,
    pub location: Option<Option<String>>,
    pub buffer_before_minutes: Option<Option<i64>>,
    pub buffer_after_minutes: Option<Option<i64>>,
    pub cancellation_notice_hours: Option<Option<i64>>,
    pub cancellation_fee_percent: Option<Option<f64>>,
    pub is_active: Option<bool>,
    pub company_id: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateAvailabilityRequest {
    /// `working_hours` or `day_off`.
    pub kind: String,
    pub weekday: Option<i64>,
    pub date: Option<String>,
    pub start_minutes: Option<i64>,
    pub end_minutes: Option<i64>,
    pub break_start_minutes: Option<i64>,
    pub break_end_minutes: Option<i64>,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub reason: Option<String>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAvailabilityRequest {
    pub kind: Option<String>,
    pub weekday: Option<Option<i64>>,
    pub date: Option<Option<String>>,
    pub start_minutes: Option<Option<i64>>,
    pub end_minutes: Option<Option<i64>>,
    pub break_start_minutes: Option<Option<i64>>,
    pub break_end_minutes: Option<Option<i64>>,
    pub valid_from: Option<Option<String>>,
    pub valid_until: Option<Option<String>>,
    pub reason: Option<Option<String>>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct AssignStaffServiceRequest {
    pub service_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct CreateAppointmentRequest {
    pub service_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staff_user_id: Option<String>,
    /// The customer to book for. Defaults to the caller. Booking
    /// for another user requires the `appointments.manage`
    /// capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_user_id: Option<String>,
    pub customer_name: String,
    pub customer_email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_phone: Option<String>,
    pub starts_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GuestBookingRequest {
    pub service_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staff_user_id: Option<String>,
    pub customer_name: String,
    pub customer_email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_phone: Option<String>,
    pub starts_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Honeypot field: a real booking form leaves it empty. Any
    /// non-empty value is treated as automated spam and rejected
    /// with the same generic error as any other failed booking, so
    /// the field's purpose is never revealed to the caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub website: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAppointmentRequest {
    pub customer_name: Option<String>,
    pub customer_email: Option<String>,
    pub customer_phone: Option<Option<String>>,
    pub notes: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct RescheduleRequest {
    pub starts_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staff_user_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CancelRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateReminderRequest {
    pub offset_minutes: i64,
    #[serde(default)]
    pub channels: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateReminderRuleRequest {
    pub name: String,
    pub offset_minutes: i64,
    #[serde(default)]
    pub channels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_id: Option<Uuid>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    pub default_slot_minutes: Option<i64>,
    pub default_buffer_before_minutes: Option<i64>,
    pub default_buffer_after_minutes: Option<i64>,
    pub default_cancellation_notice_hours: Option<i64>,
    pub default_cancellation_fee_percent: Option<f64>,
    pub allow_guest_booking: Option<bool>,
    pub guest_booking_rate_limit_per_hour: Option<i64>,
    pub guest_booking_max_advance_days: Option<i64>,
    pub min_advance_booking_minutes: Option<i64>,
    pub default_reminder_channels: Option<Vec<String>>,
    pub allowed_location_types: Option<Vec<String>>,
    pub require_confirmation: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct SlotsQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staff_id: Option<String>,
    pub date: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_minutes: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CalendarQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}
