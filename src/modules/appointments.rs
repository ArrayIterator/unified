// Appointments module — bookable services, staff calendars, slot
// generation, bookings with double-booking prevention, reschedule /
// cancel with a configurable cancellation policy, no-show marking,
// reminder rules, a customer-facing public booking flow, and an
// iCalendar export of a staff member's appointments.
//
// Routes live under `/api/v1/appointments`:
//
//   Services:
//     GET/POST /services — list services / create a service.
//     GET/PATCH/DELETE /services/{id}.
//     GET  /services/{id}/slots?date=YYYY-MM-DD&staff_id=&slot_minutes=
//          — bookable slots for one service on one day.
//   Staff calendars:
//     GET/POST /staff/{staff_id}/availability — working hours, breaks,
//          and days off for one staff member.
//     PATCH/DELETE /staff/{staff_id}/availability/{availability_id}.
//     GET/POST /staff/{staff_id}/services — services a staff member
//          is assigned to (assign / unassign).
//     DELETE /staff/{staff_id}/services/{service_id}.
//     GET  /staff/{staff_id}/calendar?from=&to= — appointments.
//     GET  /staff/{staff_id}/calendar.ics — iCalendar feed.
//   Appointments:
//     GET/POST / — list appointments / create a booking.
//     GET/PATCH /{id}.
//     POST /{id}/confirm, /{id}/reschedule, /{id}/cancel,
//          /{id}/complete, /{id}/no-show.
//     GET  /{id}/ics — one appointment as an RFC 5545 .ics file.
//   Reminders:
//     GET/POST /{id}/reminders — reminder rows of an appointment.
//     DELETE /{id}/reminders/{reminder_id}.
//     POST /{id}/reminders/{reminder_id}/send — send one now.
//     GET/POST /reminder-rules — configurable reminder rules.
//     DELETE /reminder-rules/{rule_id}.
//   Settings:
//     GET/PATCH /settings — module defaults (slot length, buffers,
//          cancellation policy, guest booking, rate limit, ...).
//   Public (no auth; see the public-access note below):
//     GET  /public/services — active services only.
//     GET  /public/services/{id}/slots — public slot lookup.
//     POST /public/book — guest booking (honeypot + rate limit).
//     GET  /public/bookings/{code} — look a booking up by its
//          confirmation code.
//
// HONESTY — READ THIS FIRST. What is real in this module:
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
// double-booking prevention under concurrency — the conflict check
// runs as check-then-insert in memory, so two simultaneous requests
// for the same slot can both pass; a database exclusion constraint
// (or a serialisable transaction around the check) is an integrator
// follow-up, and the migration carries a comment saying so; actual
// email / push delivery (notifications module); payment collection
// (a paid service records its price on the appointment, but no
// `payment.captured` listener confirms bookings — the payments /
// billing modules own collection); and invoice creation itself —
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
// "is this endpoint public?" helper to call from a handler — the
// check lives in the API Manager middleware / `resolve_public`
// path — so the public routes below rely on that gate: an
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
//     billing listener exists for it yet — see the honesty note).
//   - `crm.activity.requested` on completion (contract only: the CRM
//     module owns fulfilment).
//   - `notification.requested` (booking confirmations, reminders,
//     cancellations, no-show) with the notifications module's
//     payload shape: `{"kind", "user_id", "title", "body", "module":
//     "appointments", "link", "priority", "channels"}`. Guest
//     bookings have no user id, so the customer email address is
//     carried as both `user_id` and `email` in the payload — the
//     notifications module uses the `email` field for the email
//     channel.
//   - `scheduler.enqueue` with `{"kind":
//     "appointments.reminders.send", "payload": {"appointment_id",
//     "reminder_id", ...}}` when a reminder row is created.
// Events this module listens to:
//   - `appointments.reminders.send_requested` — the event the
//     scheduler emits when an `appointments.reminders.send` job
//     fires, following its naming rule (`backup.run` ->
//     `backup.run_requested`); sends the due reminder through
//     `notification.requested`.
//
// Scheduler note: `appointments.reminders.send` is NOT in the
// scheduler's `KNOWN_KINDS` list yet, so today the scheduler
// answers `{"queued": false, "error": "Unknown job kind: ..."}` and
// the reminder row records `scheduler_status: "declined"` instead
// of pretending it was scheduled. The reminder is still stored and
// stays usable (it can be sent manually through the send path);
// the follow-up is to add `"appointments.reminders.send"` to
// `KNOWN_KINDS` in `scheduler.rs` and arm the dispatch so the job
// emits `appointments.reminders.send_requested` with the reminder
// payload.
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

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use dashmap::DashMap;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_NAME_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 5000;
const MAX_EMAIL_LEN: usize = 254;
const MAX_PHONE_LEN: usize = 40;
const MAX_NOTES_LEN: usize = 5000;
const MAX_REASON_LEN: usize = 1000;
const MAX_LOCATION_LEN: usize = 200;
const MAX_CHANNEL_LEN: usize = 32;
const MAX_USER_ID_LEN: usize = 128;
/// Bounds on numeric inputs.
const MIN_DURATION_MINUTES: i64 = 5;
const MAX_DURATION_MINUTES: i64 = 1440;
const MIN_SLOT_MINUTES: i64 = 5;
const MAX_SLOT_MINUTES: i64 = 1440;
const MAX_BUFFER_MINUTES: i64 = 480;
const MAX_CAPACITY: i64 = 1000;
const MAX_PRICE: f64 = 1_000_000_000.0;
const MAX_OFFSET_MINUTES: i64 = 60 * 24 * 30;
const MAX_NOTICE_HOURS: i64 = 24 * 30;
const MAX_FEE_PERCENT: f64 = 100.0;
const MAX_RATE_LIMIT_PER_HOUR: i64 = 1000;
const MAX_ADVANCE_DAYS: i64 = 365;
/// Confirmation codes are 8 characters from this unambiguous set.
const CONFIRMATION_CODE_CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Capability names checked through the `role.check` hook.
const PERM_APPOINTMENTS_VIEW: &str = "appointments.view";
const PERM_APPOINTMENTS_CREATE: &str = "appointments.create";
const PERM_APPOINTMENTS_MANAGE: &str = "appointments.manage";

/// Hook event names this module listens to.
const REMINDERS_SEND_REQUESTED: &str = "appointments.reminders.send_requested";

/// Notification channels this module may request. The list is a
/// validation allow-list for reminder rules and settings; the
/// notifications module owns the channels themselves.
const NOTIFICATION_CHANNELS: &[&str] = &["in_app", "email", "webhook"];

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
    fn as_str(&self) -> &'static str {
        match self {
            Self::PendingConfirmation => "pending_confirmation",
            Self::Confirmed => "confirmed",
            Self::Cancelled => "cancelled",
            Self::Completed => "completed",
            Self::NoShow => "no_show",
        }
    }

    fn from_str(raw: &str) -> Option<Self> {
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
    /// Price per booking in `currency` (0 means free).
    pub price: f64,
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
    /// Snapshot of the service price at booking time.
    pub price: f64,
    pub currency: String,
    /// Fee actually charged on cancellation, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation_fee_charged: Option<f64>,
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

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateServiceRequest {
    pub name: String,
    pub description: Option<String>,
    pub duration_minutes: i64,
    pub price: f64,
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
    pub price: Option<f64>,
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

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for appointments. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000075_appointments`; until that migration is
/// executed, appointments do not survive a restart. The default
/// reminder rules seeded here are configurable defaults: an
/// administrator may delete or replace them through the
/// reminder-rules endpoints.
#[derive(Clone)]
pub struct AppointmentsState {
    pub services: Arc<DashMap<Uuid, AppointmentService>>,
    pub availability: Arc<DashMap<Uuid, StaffAvailability>>,
    pub staff_services: Arc<DashMap<Uuid, StaffService>>,
    pub appointments: Arc<DashMap<Uuid, Appointment>>,
    pub reminders: Arc<DashMap<Uuid, AppointmentReminder>>,
    pub reminder_rules: Arc<DashMap<Uuid, ReminderRule>>,
    pub settings: Arc<RwLock<AppointmentSettings>>,
    /// Guest-booking attempts per client key (IP) in the last hour,
    /// for the public rate limit. Pruned on every check.
    pub guest_attempts: Arc<DashMap<String, Vec<DateTime<Utc>>>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for AppointmentsState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppointmentsState {
    pub fn new() -> Self {
        let state = Self {
            services: Arc::new(DashMap::new()),
            availability: Arc::new(DashMap::new()),
            staff_services: Arc::new(DashMap::new()),
            appointments: Arc::new(DashMap::new()),
            reminders: Arc::new(DashMap::new()),
            reminder_rules: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(AppointmentSettings::default())),
            guest_attempts: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        };
        // Configurable defaults: 24 hours and 1 hour before, both
        // in-app and email. Editable / deletable via the API.
        let now = Utc::now();
        for (name, offset) in [("24 hours before", 1440_i64), ("1 hour before", 60_i64)] {
            let rule = ReminderRule {
                id: Uuid::now_v7(),
                name: name.to_string(),
                offset_minutes: offset,
                channels: vec!["in_app".to_string(), "email".to_string()],
                service_id: None,
                is_active: true,
                created_at: now,
                updated_at: now,
            };
            state.reminder_rules.insert(rule.id, rule);
        }
        state
    }

    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    fn settings_snapshot(&self) -> AppointmentSettings {
        self.settings.read().expect("settings lock poisoned").clone()
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape: `kind`, `user_id`, `title`, `body`,
    /// `module: "appointments"`, `link`, `priority`, `channels`.
    /// For guest bookings `user_id` is the customer email address
    /// and the same address is passed as `email` so the email
    /// channel can deliver it.
    fn notify(
        &self,
        kind: &str,
        user_id: &str,
        email: Option<&str>,
        title: &str,
        body: &str,
        link: Option<String>,
        channels: &[String],
    ) {
        let mut payload = json!({
            "kind": kind,
            "user_id": user_id,
            "title": title,
            "body": body,
            "module": "appointments",
            "category": "appointments",
            "priority": "normal",
            "link": link,
            "channels": channels,
        });
        if let Some(email) = email {
            if let Value::Object(map) = &mut payload {
                map.insert("email".to_string(), Value::String(email.to_string()));
            }
        }
        self.emit("notification.requested", payload);
    }

    /// Recipient key and email for one appointment's customer: the
    /// logged-in user id when known, otherwise the email address.
    fn customer_recipient(appointment: &Appointment) -> (String, String) {
        match &appointment.customer_user_id {
            Some(user_id) => (user_id.clone(), appointment.customer_email.clone()),
            None => (
                appointment.customer_email.clone(),
                appointment.customer_email.clone(),
            ),
        }
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

fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        Some(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else if trimmed.len() > max {
                Err(UnifiedError::bad_request(format!(
                    "{field} must be at most {max} characters"
                )))
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        None => Ok(None),
    }
}

fn valid_email(email: &str) -> UnifiedResult<String> {
    let trimmed = email.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_EMAIL_LEN || trimmed.contains(' ') {
        return Err(UnifiedError::bad_request("Invalid email address"));
    }
    let mut parts = trimmed.split('@');
    let (Some(local), Some(domain)) = (parts.next(), parts.next()) else {
        return Err(UnifiedError::bad_request("Invalid email address"));
    };
    if parts.next().is_some() || local.is_empty() || domain.is_empty() {
        return Err(UnifiedError::bad_request("Invalid email address"));
    }
    if !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') {
        return Err(UnifiedError::bad_request("Invalid email address"));
    }
    Ok(trimmed.to_string())
}

fn valid_user_id(value: &str, field: &str) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_USER_ID_LEN {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be empty and must be at most {MAX_USER_ID_LEN} characters"
        )));
    }
    Ok(trimmed.to_string())
}

/// Money: finite, non-negative, bounded, rounded to 4 decimals.
fn valid_price(price: f64) -> UnifiedResult<f64> {
    if !price.is_finite() || price < 0.0 || price > MAX_PRICE {
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

fn valid_channels(channels: &[String]) -> UnifiedResult<Vec<String>> {
    if channels.is_empty() {
        return Err(UnifiedError::bad_request("At least one channel is required"));
    }
    let mut out = Vec::with_capacity(channels.len());
    for channel in channels {
        let trimmed = channel.trim();
        if trimmed.len() > MAX_CHANNEL_LEN || !NOTIFICATION_CHANNELS.contains(&trimmed) {
            return Err(UnifiedError::bad_request(format!(
                "Channels must be a subset of: {}",
                NOTIFICATION_CHANNELS.join(", ")
            )));
        }
        if !out.contains(&trimmed.to_string()) {
            out.push(trimmed.to_string());
        }
    }
    Ok(out)
}

fn parse_date(raw: &str, field: &str) -> UnifiedResult<NaiveDate> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be a date in YYYY-MM-DD format")))
}

fn parse_datetime(raw: &str, field: &str) -> UnifiedResult<DateTime<Utc>> {
    raw.trim()
        .parse::<DateTime<Utc>>()
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be an RFC 3339 date-time")))
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

fn confirmation_code() -> String {
    let mut rng = rand::rng();
    (0..8)
        .map(|_| {
            let index = rng.random_range(0..CONFIRMATION_CODE_CHARS.len());
            CONFIRMATION_CODE_CHARS[index] as char
        })
        .collect()
}

/// Client key for the public rate limit: the first address in
/// `X-Forwarded-For`, else `X-Real-IP`, else "unknown". The key is
/// never stored beyond the last hour's attempt timestamps.
fn client_key(req: &HttpRequest) -> String {
    req.headers()
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            req.headers()
                .get("x-real-ip")
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or("unknown")
        .to_string()
}

/// Record one public booking attempt for `key` and fail with 429
/// when the hourly limit from the settings is reached. Attempts
/// older than one hour are pruned on every call.
fn check_guest_rate_limit(state: &AppointmentsState, key: &str, limit: i64) -> UnifiedResult<()> {
    let now = Utc::now();
    let cutoff = now - Duration::hours(1);
    let mut entry = state.guest_attempts.entry(key.to_string()).or_default();
    entry.retain(|instant| *instant > cutoff);
    if entry.len() as i64 >= limit.max(1) {
        return Err(UnifiedError::new(
            "Too many booking attempts. Please try again later.",
            429,
        ));
    }
    entry.push(now);
    Ok(())
}

// ---------------------------------------------------------------------------
// Loaders
// ---------------------------------------------------------------------------

fn load_service(state: &AppointmentsState, raw_id: &str) -> UnifiedResult<AppointmentService> {
    let id = parse_uuid(raw_id, "service")?;
    state
        .services
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Service not found: {id}")))
}

fn load_service_by_id(state: &AppointmentsState, id: Uuid) -> UnifiedResult<AppointmentService> {
    state
        .services
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Service not found: {id}")))
}

fn load_appointment(state: &AppointmentsState, raw_id: &str) -> UnifiedResult<Appointment> {
    let id = parse_uuid(raw_id, "appointment")?;
    state
        .appointments
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Appointment not found: {id}")))
}

fn load_availability(
    state: &AppointmentsState,
    staff_user_id: &str,
    raw_id: &str,
) -> UnifiedResult<StaffAvailability> {
    let id = parse_uuid(raw_id, "availability")?;
    state
        .availability
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|row| row.staff_user_id == staff_user_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Availability not found: {id}")))
}

fn load_reminder(state: &AppointmentsState, raw_id: &str) -> UnifiedResult<AppointmentReminder> {
    let id = parse_uuid(raw_id, "reminder")?;
    state
        .reminders
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Reminder not found: {id}")))
}

fn load_rule(state: &AppointmentsState, raw_id: &str) -> UnifiedResult<ReminderRule> {
    let id = parse_uuid(raw_id, "reminder rule")?;
    state
        .reminder_rules
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Reminder rule not found: {id}")))
}

// ---------------------------------------------------------------------------
// Availability, slots, and conflicts
// ---------------------------------------------------------------------------

/// Effective buffer / cancellation values for one service: the
/// per-service override wins, otherwise the module setting.
fn effective_buffer_before(service: &AppointmentService, settings: &AppointmentSettings) -> i64 {
    service
        .buffer_before_minutes
        .unwrap_or(settings.default_buffer_before_minutes)
}

fn effective_buffer_after(service: &AppointmentService, settings: &AppointmentSettings) -> i64 {
    service
        .buffer_after_minutes
        .unwrap_or(settings.default_buffer_after_minutes)
}

fn effective_notice_hours(service: &AppointmentService, settings: &AppointmentSettings) -> i64 {
    service
        .cancellation_notice_hours
        .unwrap_or(settings.default_cancellation_notice_hours)
}

fn effective_fee_percent(service: &AppointmentService, settings: &AppointmentSettings) -> f64 {
    service
        .cancellation_fee_percent
        .unwrap_or(settings.default_cancellation_fee_percent)
}

/// Appointments that block a slot: pending confirmation or
/// confirmed. Cancelled, completed, and no-show appointments free
/// the time again.
fn is_blocking(status: AppointmentStatus) -> bool {
    matches!(
        status,
        AppointmentStatus::PendingConfirmation | AppointmentStatus::Confirmed
    )
}

/// True when the half-open windows [a_start, a_end) and
/// [b_start, b_end) overlap.
fn windows_overlap(
    a_start: DateTime<Utc>,
    a_end: DateTime<Utc>,
    b_start: DateTime<Utc>,
    b_end: DateTime<Utc>,
) -> bool {
    a_start < b_end && b_start < a_end
}

/// Buffered window of one appointment: its own service's buffers
/// extend the occupied time on both sides.
fn buffered_window(
    state: &AppointmentsState,
    appointment: &Appointment,
    settings: &AppointmentSettings,
) -> (DateTime<Utc>, DateTime<Utc>) {
    let (before, after) = match state.services.get(&appointment.service_id) {
        Some(service) => (
            effective_buffer_before(service.value(), settings),
            effective_buffer_after(service.value(), settings),
        ),
        None => (
            settings.default_buffer_before_minutes,
            settings.default_buffer_after_minutes,
        ),
    };
    (
        appointment.starts_at - Duration::minutes(before),
        appointment.ends_at + Duration::minutes(after),
    )
}

/// Server-side double-booking check for a requested window.
/// Returns a human-readable conflict reason when the booking must
/// be refused, or `None` when the window is free.
///
/// HONESTY: this is a check-then-insert in memory. Two requests
/// that pass the check at the same instant can both insert, so
/// this prevents ordinary double-booking but is not an atomic
/// guarantee. A database exclusion constraint (or a serialisable
/// transaction around the check) is an integrator follow-up; the
/// migration carries the same note.
fn find_conflict(
    state: &AppointmentsState,
    service: &AppointmentService,
    staff_user_id: Option<&str>,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    except_id: Option<Uuid>,
) -> Option<String> {
    let settings = state.settings_snapshot();
    let new_before = effective_buffer_before(service, &settings);
    let new_after = effective_buffer_after(service, &settings);
    let new_start = starts_at - Duration::minutes(new_before);
    let new_end = ends_at + Duration::minutes(new_after);

    // One staff member cannot be in two places at once (buffers
    // included on both sides).
    if let Some(staff) = staff_user_id {
        for entry in state.appointments.iter() {
            let existing = entry.value();
            if Some(existing.id) == except_id || !is_blocking(existing.status) {
                continue;
            }
            if existing.staff_user_id.as_deref() != Some(staff) {
                continue;
            }
            let (existing_start, existing_end) = buffered_window(state, existing, &settings);
            if windows_overlap(new_start, new_end, existing_start, existing_end) {
                return Some(format!(
                    "Staff member already has a booking overlapping {}",
                    existing.starts_at.to_rfc3339()
                ));
            }
        }
    }

    // Group services: the number of overlapping bookings for the
    // service must stay below its capacity.
    let overlapping: i64 = state
        .appointments
        .iter()
        .filter(|entry| {
            let existing = entry.value();
            Some(existing.id) != except_id
                && is_blocking(existing.status)
                && existing.service_id == service.id
                && windows_overlap(starts_at, ends_at, existing.starts_at, existing.ends_at)
        })
        .count() as i64;
    if overlapping >= service.capacity {
        return Some(format!(
            "Service \"{}\" is fully booked for the requested time",
            service.name
        ));
    }
    None
}

/// Validate a requested start time against the advance-booking
/// rules: far enough in the future, and not beyond the configured
/// booking horizon.
fn validate_start_time(
    settings: &AppointmentSettings,
    starts_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> UnifiedResult<()> {
    let earliest = now + Duration::minutes(settings.min_advance_booking_minutes);
    if starts_at < earliest {
        return Err(UnifiedError::bad_request(format!(
            "Bookings must be made at least {} minute(s) in advance",
            settings.min_advance_booking_minutes
        )));
    }
    let latest = now + Duration::days(settings.guest_booking_max_advance_days);
    if starts_at > latest {
        return Err(UnifiedError::bad_request(format!(
            "Bookings cannot be made more than {} day(s) in advance",
            settings.guest_booking_max_advance_days
        )));
    }
    Ok(())
}

/// True when the staff member is assigned to the service and the
/// assignment is active.
fn staff_assigned_to(state: &AppointmentsState, staff_user_id: &str, service_id: Uuid) -> bool {
    state.staff_services.iter().any(|entry| {
        let row = entry.value();
        row.staff_user_id == staff_user_id && row.service_id == service_id && row.is_active
    })
}

/// Staff members assigned to a service (active assignments only).
fn staff_for_service(state: &AppointmentsState, service_id: Uuid) -> Vec<String> {
    let mut staff: Vec<String> = state
        .staff_services
        .iter()
        .filter(|entry| {
            let row = entry.value();
            row.service_id == service_id && row.is_active
        })
        .map(|entry| entry.value().staff_user_id.clone())
        .collect();
    staff.sort();
    staff.dedup();
    staff
}

/// True when the staff member has a day off on `date`.
fn has_day_off(state: &AppointmentsState, staff_user_id: &str, date: NaiveDate) -> bool {
    state.availability.iter().any(|entry| {
        let row = entry.value();
        row.staff_user_id == staff_user_id
            && row.kind == "day_off"
            && row.is_active
            && row.date == Some(date)
    })
}

/// Working-hours windows for one staff member on one weekday
/// (0 = Monday .. 6 = Sunday), limited to rows whose validity
/// range contains `date`. Each window is (start, end, break) in
/// minutes from midnight (UTC).
fn working_windows(
    state: &AppointmentsState,
    staff_user_id: &str,
    weekday: i64,
    date: NaiveDate,
) -> Vec<(i64, i64, Option<(i64, i64)>)> {
    let mut windows: Vec<(i64, i64, Option<(i64, i64)>)> = state
        .availability
        .iter()
        .filter(|entry| {
            let row = entry.value();
            row.staff_user_id == staff_user_id
                && row.kind == "working_hours"
                && row.is_active
                && row.weekday == Some(weekday)
                && row.valid_from.is_none_or(|from| date >= from)
                && row.valid_until.is_none_or(|until| date <= until)
        })
        .filter_map(|entry| {
            let row = entry.value();
            match (row.start_minutes, row.end_minutes) {
                (Some(start), Some(end)) if start < end => {
                    let break_window = match (row.break_start_minutes, row.break_end_minutes) {
                        (Some(break_start), Some(break_end)) if break_start < break_end => {
                            Some((break_start, break_end))
                        }
                        _ => None,
                    };
                    Some((start, end, break_window))
                }
                _ => None,
            }
        })
        .collect();
    windows.sort();
    windows.dedup();
    windows
}

/// Generate the bookable slots for one service on one day, for the
/// given staff members. A slot starts every `slot_minutes` from the
/// start of a working window, lasts exactly the service duration,
/// and is kept only when it fits inside the window, avoids breaks,
/// does not overlap a buffered existing booking for that staff
/// member, and does not exceed the service capacity.
fn generate_slots(
    state: &AppointmentsState,
    service: &AppointmentService,
    staff_ids: &[String],
    date: NaiveDate,
    slot_minutes: i64,
) -> Vec<BookingSlot> {
    let weekday = date.weekday().num_days_from_monday() as i64;
    let day_start = date
        .and_hms_opt(0, 0, 0)
        .map(|naive| Utc.from_utc_datetime(&naive))
        .unwrap_or_else(Utc::now);
    let mut slots = Vec::new();
    for staff in staff_ids {
        if has_day_off(state, staff, date) {
            continue;
        }
        for (window_start, window_end, break_window) in working_windows(state, staff, weekday, date)
        {
            let mut cursor = window_start;
            while cursor + service.duration_minutes <= window_end {
                let starts_at = day_start + Duration::minutes(cursor);
                let ends_at = starts_at + Duration::minutes(service.duration_minutes);
                let slot_start_min = cursor;
                let slot_end_min = cursor + service.duration_minutes;
                cursor += slot_minutes;
                // A slot that touches a break is not bookable.
                if let Some((break_start, break_end)) = break_window {
                    if slot_start_min < break_end && break_start < slot_end_min {
                        continue;
                    }
                }
                if find_conflict(state, service, Some(staff), starts_at, ends_at, None).is_some() {
                    continue;
                }
                slots.push(BookingSlot {
                    starts_at,
                    ends_at,
                    staff_user_id: staff.clone(),
                    service_id: service.id,
                });
            }
        }
    }
    slots.sort_by_key(|slot| (slot.starts_at, slot.staff_user_id.clone()));
    slots
}

/// Cancellation fee for one appointment at `now`: zero when the
/// cancellation is outside the notice window, otherwise the
/// configured percentage of the booked price.
fn cancellation_fee(
    service: &AppointmentService,
    settings: &AppointmentSettings,
    appointment: &Appointment,
    now: DateTime<Utc>,
) -> f64 {
    let notice = effective_notice_hours(service, settings);
    let deadline = appointment.starts_at - Duration::hours(notice);
    if now <= deadline {
        0.0
    } else {
        round_money(appointment.price * effective_fee_percent(service, settings) / 100.0)
    }
}

// ---------------------------------------------------------------------------
// Reminders
// ---------------------------------------------------------------------------

/// Queue one reminder through `scheduler.enqueue` with kind
/// `appointments.reminders.send`. The scheduler answers
/// `{"queued": true, "job_id": ...}` when it knows the kind, and
/// `{"queued": false, "error": "Unknown job kind: ..."}` today,
/// because `appointments.reminders.send` is not in `KNOWN_KINDS`
/// yet. The reminder is stored either way; `scheduler_status`
/// records the outcome honestly (`queued` / `declined`).
fn schedule_reminder(state: &AppointmentsState, reminder: &mut AppointmentReminder, appointment: &Appointment) {
    let Some(bus) = state.bus() else {
        reminder.scheduler_job_id = None;
        reminder.scheduler_status = "declined".to_string();
        return;
    };
    let send_at = appointment.starts_at - Duration::minutes(reminder.offset_minutes);
    let answer = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": "appointments.reminders.send",
            "payload": {
                "appointment_id": appointment.id.to_string(),
                "reminder_id": reminder.id.to_string(),
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

/// Materialise one reminder row per active reminder rule that
/// applies to the appointment's service (a rule with
/// `service_id: None` applies to all services). Existing pending
/// rows for the same rule offset are not duplicated.
fn materialise_reminders(state: &AppointmentsState, appointment: &Appointment) {
    let now = Utc::now();
    let mut rules: Vec<ReminderRule> = state
        .reminder_rules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rule| {
            rule.is_active
                && (rule.service_id.is_none() || rule.service_id == Some(appointment.service_id))
        })
        .collect();
    rules.sort_by_key(|rule| rule.offset_minutes);
    for rule in rules {
        let already = state.reminders.iter().any(|entry| {
            let row = entry.value();
            row.appointment_id == appointment.id
                && row.offset_minutes == rule.offset_minutes
                && row.status == "pending"
        });
        if already {
            continue;
        }
        let mut reminder = AppointmentReminder {
            id: Uuid::now_v7(),
            appointment_id: appointment.id,
            offset_minutes: rule.offset_minutes,
            channels: rule.channels.clone(),
            status: "pending".to_string(),
            scheduler_job_id: None,
            scheduler_status: "not_scheduled".to_string(),
            sent_at: None,
            created_at: now,
            updated_at: now,
        };
        schedule_reminder(state, &mut reminder, appointment);
        state.reminders.insert(reminder.id, reminder);
    }
}

/// Cancel every pending reminder row of an appointment (used on
/// cancellation and before re-materialising after a reschedule).
fn cancel_pending_reminders(state: &AppointmentsState, appointment_id: Uuid) {
    let now = Utc::now();
    for mut entry in state.reminders.iter_mut() {
        if entry.value().appointment_id == appointment_id && entry.value().status == "pending" {
            entry.value_mut().status = "cancelled".to_string();
            entry.value_mut().updated_at = now;
        }
    }
}

/// Send one reminder now through `notification.requested`: one
/// notification to the customer and one to the staff member. The
/// reminder row is marked `sent` (or `failed` when the appointment
/// it belongs to is gone).
fn send_reminder(state: &AppointmentsState, reminder: &AppointmentReminder) -> UnifiedResult<usize> {
    let appointment = state
        .appointments
        .get(&reminder.appointment_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| {
            UnifiedError::not_found(format!("Appointment not found: {}", reminder.appointment_id))
        })?;
    let service = load_service_by_id(state, appointment.service_id)?;
    let (customer_id, customer_email) = AppointmentsState::customer_recipient(&appointment);
    let title = format!("Reminder: {} at {}", service.name, appointment.starts_at.to_rfc3339());
    let body = format!(
        "Your appointment \"{}\" starts at {}. Confirmation code: {}.",
        service.name,
        appointment.starts_at.to_rfc3339(),
        appointment.confirmation_code
    );
    let link = format!("/api/v1/appointments/public/bookings/{}", appointment.confirmation_code);
    state.notify(
        "appointments.reminder",
        &customer_id,
        Some(&customer_email),
        &title,
        &body,
        Some(link),
        &reminder.channels,
    );
    let mut sent = 1;
    if let Some(staff) = &appointment.staff_user_id {
        state.notify(
            "appointments.reminder",
            staff,
            None,
            &title,
            &body,
            None,
            &reminder.channels,
        );
        sent += 1;
    }
    Ok(sent)
}

/// The scheduler's `appointments.reminders.send_requested` handler:
/// send the due reminder through `notification.requested` and mark
/// the row `sent` (or `failed` when the appointment is gone).
fn on_reminders_send_requested(state: AppointmentsState, ctx: &mut HookContext) {
    let reminder_id = ctx
        .get("reminder_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let Ok(reminder) = load_reminder(&state, &reminder_id) else {
        ctx.add_result(json!({ "error": format!("Reminder not found: {reminder_id}") }));
        return;
    };
    if reminder.status != "pending" {
        ctx.add_result(json!({ "reminder_id": reminder.id, "skipped": true, "status": reminder.status }));
        return;
    }
    match send_reminder(&state, &reminder) {
        Ok(sent) => {
            let now = Utc::now();
            if let Some(mut row) = state.reminders.get_mut(&reminder.id) {
                row.value_mut().status = "sent".to_string();
                row.value_mut().sent_at = Some(now);
                row.value_mut().updated_at = now;
            }
            state.emit(
                "appointments.reminder.sent",
                json!({
                    "reminder_id": reminder.id,
                    "appointment_id": reminder.appointment_id,
                    "sent": sent,
                    "sent_at": now.to_rfc3339(),
                }),
            );
            ctx.add_result(json!({ "reminder_id": reminder.id, "sent": sent }));
        }
        Err(error) => {
            let now = Utc::now();
            if let Some(mut row) = state.reminders.get_mut(&reminder.id) {
                row.value_mut().status = "failed".to_string();
                row.value_mut().updated_at = now;
            }
            ctx.add_result(json!({ "reminder_id": reminder.id, "error": error.message }));
        }
    }
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

fn ics_appointment_block(
    appointment: &Appointment,
    service: &AppointmentService,
) -> String {
    let mut lines: Vec<String> = vec![
        "BEGIN:VEVENT".to_string(),
        format!("UID:{}@appointments.unified", appointment.id),
        format!("DTSTAMP:{}", ics_datetime(Utc::now())),
        format!("DTSTART:{}", ics_datetime(appointment.starts_at)),
        format!("DTEND:{}", ics_datetime(appointment.ends_at)),
        format!(
            "SUMMARY:{}",
            ics_escape(&format!("{} — {}", service.name, appointment.customer_name))
        ),
    ];
    let description = format!(
        "Customer: {} ({})\nConfirmation code: {}",
        appointment.customer_name, appointment.customer_email, appointment.confirmation_code
    );
    lines.push(format!("DESCRIPTION:{}", ics_escape(&description)));
    if let Some(location) = &service.location {
        if !location.trim().is_empty() {
            lines.push(format!("LOCATION:{}", ics_escape(location)));
        }
    }
    match appointment.status {
        AppointmentStatus::Cancelled => lines.push("STATUS:CANCELLED".to_string()),
        AppointmentStatus::Completed => lines.push("STATUS:CONFIRMED".to_string()),
        _ => lines.push("STATUS:CONFIRMED".to_string()),
    }
    lines.push("END:VEVENT".to_string());
    lines
        .into_iter()
        .map(|line| ics_fold(&line))
        .collect::<Vec<_>>()
        .join("\r\n")
}

fn build_ics(blocks: &[String], calendar_name: &str) -> String {
    let mut out = String::new();
    out.push_str("BEGIN:VCALENDAR\r\n");
    out.push_str("VERSION:2.0\r\n");
    out.push_str("PRODID:-//Unified//Appointments//EN\r\n");
    out.push_str("CALSCALE:GREGORIAN\r\n");
    out.push_str("METHOD:PUBLISH\r\n");
    out.push_str(&ics_fold(&format!("X-WR-CALNAME:{}", ics_escape(calendar_name))));
    out.push_str("\r\n");
    for block in blocks {
        out.push_str(block);
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

/// Public view of one appointment: the confirmation code is the
/// only credential, so the customer phone number is never included.
fn public_appointment_view(appointment: &Appointment, service: &AppointmentService) -> Value {
    json!({
        "id": appointment.id,
        "service_id": appointment.service_id,
        "service_name": service.name,
        "staff_user_id": appointment.staff_user_id,
        "customer_name": appointment.customer_name,
        "customer_email": appointment.customer_email,
        "starts_at": appointment.starts_at.to_rfc3339(),
        "ends_at": appointment.ends_at.to_rfc3339(),
        "status": appointment.status.as_str(),
        "price": appointment.price,
        "currency": appointment.currency,
        "cancellation_fee_charged": appointment.cancellation_fee_charged,
        "confirmation_code": appointment.confirmation_code,
        "created_at": appointment.created_at.to_rfc3339(),
        "updated_at": appointment.updated_at.to_rfc3339(),
    })
}

// ---------------------------------------------------------------------------
// Handlers: services
// ---------------------------------------------------------------------------

fn validate_service_fields(
    duration_minutes: i64,
    price: f64,
    capacity: i64,
    buffer_before: Option<i64>,
    buffer_after: Option<i64>,
    notice_hours: Option<i64>,
    fee_percent: Option<f64>,
) -> UnifiedResult<f64> {
    if !(MIN_DURATION_MINUTES..=MAX_DURATION_MINUTES).contains(&duration_minutes) {
        return Err(UnifiedError::bad_request(format!(
            "duration_minutes must be between {MIN_DURATION_MINUTES} and {MAX_DURATION_MINUTES}"
        )));
    }
    let price = valid_price(price)?;
    if !(1..=MAX_CAPACITY).contains(&capacity) {
        return Err(UnifiedError::bad_request(format!(
            "capacity must be between 1 and {MAX_CAPACITY}"
        )));
    }
    for (value, field) in [
        (buffer_before, "buffer_before_minutes"),
        (buffer_after, "buffer_after_minutes"),
    ] {
        if let Some(minutes) = value {
            if !(0..=MAX_BUFFER_MINUTES).contains(&minutes) {
                return Err(UnifiedError::bad_request(format!(
                    "{field} must be between 0 and {MAX_BUFFER_MINUTES}"
                )));
            }
        }
    }
    if let Some(hours) = notice_hours {
        if !(0..=MAX_NOTICE_HOURS).contains(&hours) {
            return Err(UnifiedError::bad_request(format!(
                "cancellation_notice_hours must be between 0 and {MAX_NOTICE_HOURS}"
            )));
        }
    }
    if let Some(percent) = fee_percent {
        if !percent.is_finite() || !(0.0..=MAX_FEE_PERCENT).contains(&percent) {
            return Err(UnifiedError::bad_request(
                "cancellation_fee_percent must be between 0 and 100",
            ));
        }
    }
    Ok(price)
}

async fn list_services(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<AppointmentService> = state
            .services
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|service| {
                params
                    .filters
                    .get("is_active")
                    .is_none_or(|value| service.is_active == (*value == "true" || *value == "1"))
                    && params.filters.get("location_type").is_none_or(|value| {
                        service.location_type == *value
                    })
                    && params.filters.get("company_id").is_none_or(|value| {
                        service.company_id.as_deref() == Some(value.as_str())
                    })
                    && params.search.as_ref().is_none_or(|needle| {
                        service
                            .name
                            .to_lowercase()
                            .contains(&needle.to_lowercase())
                            || service
                                .description
                                .as_deref()
                                .unwrap_or("")
                                .to_lowercase()
                                .contains(&needle.to_lowercase())
                    })
            })
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

async fn create_service(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    body: web::Json<CreateServiceRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AppointmentService> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let input = body.into_inner();
        let settings = state.settings_snapshot();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let price = validate_service_fields(
            input.duration_minutes,
            input.price,
            input.capacity.unwrap_or(1),
            input.buffer_before_minutes,
            input.buffer_after_minutes,
            input.cancellation_notice_hours,
            input.cancellation_fee_percent,
        )?;
        let currency = valid_currency(input.currency.as_deref().unwrap_or("USD"))?;
        let location_type = input.location_type.unwrap_or_else(|| "in_person".to_string());
        if !settings.allowed_location_types.contains(&location_type) {
            return Err(UnifiedError::bad_request(format!(
                "location_type must be one of: {}",
                settings.allowed_location_types.join(", ")
            )));
        }
        let location = optional_text(&input.location, "location", MAX_LOCATION_LEN)?;
        let company_id = optional_text(&input.company_id, "company_id", MAX_USER_ID_LEN)?;
        let now = Utc::now();
        let service = AppointmentService {
            id: Uuid::now_v7(),
            name,
            description,
            duration_minutes: input.duration_minutes,
            price,
            currency,
            capacity: input.capacity.unwrap_or(1),
            requires_staff: input.requires_staff.unwrap_or(true),
            location_type,
            location,
            buffer_before_minutes: input.buffer_before_minutes,
            buffer_after_minutes: input.buffer_after_minutes,
            cancellation_notice_hours: input.cancellation_notice_hours,
            cancellation_fee_percent: input.cancellation_fee_percent,
            is_active: input.is_active.unwrap_or(true),
            company_id,
            created_at: now,
            updated_at: now,
        };
        state.services.insert(service.id, service.clone());
        state.emit(
            "appointments.service.created",
            json!({ "service_id": service.id, "name": service.name }),
        );
        Ok(service)
    })();
    match result {
        Ok(service) => created(service),
        Err(error) => err(error, debug),
    }
}

async fn get_service(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_VIEW)?;
        load_service(&state, &path.into_inner())
    })();
    match result {
        Ok(service) => ok(service),
        Err(error) => err(error, debug),
    }
}

async fn update_service(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateServiceRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AppointmentService> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let input = body.into_inner();
        let settings = state.settings_snapshot();
        let mut service = load_service(&state, &path.into_inner())?;
        if let Some(name) = &input.name {
            service.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            service.description = optional_text(&Some(description.clone()), "description", MAX_DESCRIPTION_LEN)?;
        }
        if let Some(duration) = input.duration_minutes {
            service.duration_minutes = duration;
        }
        if let Some(price) = input.price {
            service.price = price;
        }
        if let Some(currency) = &input.currency {
            service.currency = valid_currency(currency)?;
        }
        if let Some(capacity) = input.capacity {
            service.capacity = capacity;
        }
        if let Some(requires_staff) = input.requires_staff {
            service.requires_staff = requires_staff;
        }
        if let Some(location_type) = &input.location_type {
            if !settings.allowed_location_types.contains(location_type) {
                return Err(UnifiedError::bad_request(format!(
                    "location_type must be one of: {}",
                    settings.allowed_location_types.join(", ")
                )));
            }
            service.location_type = location_type.clone();
        }
        if let Some(location) = &input.location {
            service.location = optional_text(&Some(location.clone()), "location", MAX_LOCATION_LEN)?;
        }
        if let Some(buffer) = input.buffer_before_minutes {
            service.buffer_before_minutes = buffer;
        }
        if let Some(buffer) = input.buffer_after_minutes {
            service.buffer_after_minutes = buffer;
        }
        if let Some(notice) = input.cancellation_notice_hours {
            service.cancellation_notice_hours = notice;
        }
        if let Some(fee) = input.cancellation_fee_percent {
            service.cancellation_fee_percent = fee;
        }
        if let Some(is_active) = input.is_active {
            service.is_active = is_active;
        }
        if let Some(company_id) = &input.company_id {
            service.company_id = optional_text(&Some(company_id.clone()), "company_id", MAX_USER_ID_LEN)?;
        }
        validate_service_fields(
            service.duration_minutes,
            service.price,
            service.capacity,
            service.buffer_before_minutes,
            service.buffer_after_minutes,
            service.cancellation_notice_hours,
            service.cancellation_fee_percent,
        )?;
        service.price = valid_price(service.price)?;
        service.updated_at = Utc::now();
        state.services.insert(service.id, service.clone());
        state.emit(
            "appointments.service.updated",
            json!({ "service_id": service.id, "name": service.name }),
        );
        Ok(service)
    })();
    match result {
        Ok(service) => ok(service),
        Err(error) => err(error, debug),
    }
}

async fn delete_service(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let service = load_service(&state, &path.into_inner())?;
        let blocking = state.appointments.iter().any(|entry| {
            let appointment = entry.value();
            appointment.service_id == service.id && is_blocking(appointment.status)
        });
        if blocking {
            return Err(UnifiedError::conflict(
                "Service has active bookings and cannot be deleted; deactivate it instead",
            ));
        }
        state.services.remove(&service.id);
        let stale: Vec<Uuid> = state
            .staff_services
            .iter()
            .filter(|entry| entry.value().service_id == service.id)
            .map(|entry| *entry.key())
            .collect();
        for id in stale {
            state.staff_services.remove(&id);
        }
        state.emit(
            "appointments.service.deleted",
            json!({ "service_id": service.id, "name": service.name }),
        );
        Ok(json!({ "deleted": true, "id": service.id }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Handlers: slots (staff + public share the generator)
// ---------------------------------------------------------------------------

fn slots_for_service(
    state: &AppointmentsState,
    service_id: &str,
    query: &SlotsQuery,
) -> UnifiedResult<Vec<BookingSlot>> {
    let service = load_service(state, service_id)?;
    if !service.is_active {
        return Err(UnifiedError::not_found(format!("Service not found: {service_id}")));
    }
    let settings = state.settings_snapshot();
    let date = parse_date(&query.date, "date")?;
    let slot_minutes = query.slot_minutes.unwrap_or(settings.default_slot_minutes);
    if !(MIN_SLOT_MINUTES..=MAX_SLOT_MINUTES).contains(&slot_minutes) {
        return Err(UnifiedError::bad_request(format!(
            "slot_minutes must be between {MIN_SLOT_MINUTES} and {MAX_SLOT_MINUTES}"
        )));
    }
    if slot_minutes < service.duration_minutes {
        return Err(UnifiedError::bad_request(
            "slot_minutes must be at least the service duration",
        ));
    }
    let staff_ids: Vec<String> = match &query.staff_id {
        Some(staff) => {
            let staff = valid_user_id(staff, "staff_id")?;
            if !staff_assigned_to(state, &staff, service.id) {
                return Err(UnifiedError::not_found(
                    "Staff member is not assigned to this service",
                ));
            }
            vec![staff]
        }
        None => staff_for_service(state, service.id),
    };
    Ok(generate_slots(state, &service, &staff_ids, date, slot_minutes))
}

async fn list_service_slots(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<SlotsQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_VIEW)?;
        let slots = slots_for_service(&state, &path.into_inner(), &query.into_inner())?;
        Ok(json!({ "slots": slots, "count": slots.len() }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

async fn list_public_services(
    state: web::Data<AppointmentsState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<AppointmentService> = state
        .services
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|service| {
            service.is_active
                && params.filters.get("location_type").is_none_or(|value| {
                    service.location_type == *value
                })
                && params.search.as_ref().is_none_or(|needle| {
                    service
                        .name
                        .to_lowercase()
                        .contains(&needle.to_lowercase())
                })
        })
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name));
    let page = paginate(items, &params);
    ok(serde_json::to_value(page).unwrap_or(Value::Null))
}

async fn list_public_service_slots(
    state: web::Data<AppointmentsState>,
    path: web::Path<String>,
    query: web::Query<SlotsQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let slots = slots_for_service(&state, &path.into_inner(), &query.into_inner())?;
        Ok(json!({ "slots": slots, "count": slots.len() }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, false),
    }
}

// ---------------------------------------------------------------------------
// Handlers: staff availability
// ---------------------------------------------------------------------------

fn validate_availability(
    kind: &str,
    weekday: Option<i64>,
    date: Option<NaiveDate>,
    start_minutes: Option<i64>,
    end_minutes: Option<i64>,
    break_start: Option<i64>,
    break_end: Option<i64>,
    valid_from: Option<NaiveDate>,
    valid_until: Option<NaiveDate>,
) -> UnifiedResult<()> {
    match kind {
        "working_hours" => {
            let weekday = weekday.ok_or_else(|| {
                UnifiedError::bad_request("weekday (0 = Monday .. 6 = Sunday) is required for working_hours")
            })?;
            if !(0..=6).contains(&weekday) {
                return Err(UnifiedError::bad_request("weekday must be between 0 and 6"));
            }
            let (Some(start), Some(end)) = (start_minutes, end_minutes) else {
                return Err(UnifiedError::bad_request(
                    "start_minutes and end_minutes are required for working_hours",
                ));
            };
            if !(0..=1440).contains(&start) || !(0..=1440).contains(&end) || start >= end {
                return Err(UnifiedError::bad_request(
                    "start_minutes must be < end_minutes and both within 0..=1440",
                ));
            }
            match (break_start, break_end) {
                (Some(break_start), Some(break_end)) => {
                    if break_start < start || break_end > end || break_start >= break_end {
                        return Err(UnifiedError::bad_request(
                            "break must lie inside the working window with break_start < break_end",
                        ));
                    }
                }
                (None, None) => {}
                _ => {
                    return Err(UnifiedError::bad_request(
                        "break_start_minutes and break_end_minutes must be given together",
                    ));
                }
            }
            if date.is_some() {
                return Err(UnifiedError::bad_request(
                    "date must not be given for working_hours; use day_off for a single date",
                ));
            }
        }
        "day_off" => {
            if date.is_none() {
                return Err(UnifiedError::bad_request("date is required for day_off"));
            }
            if weekday.is_some() || start_minutes.is_some() || end_minutes.is_some() {
                return Err(UnifiedError::bad_request(
                    "weekday and start/end minutes must not be given for day_off",
                ));
            }
        }
        other => {
            return Err(UnifiedError::bad_request(format!(
                "kind must be \"working_hours\" or \"day_off\", not \"{other}\""
            )));
        }
    }
    if let (Some(from), Some(until)) = (valid_from, valid_until) {
        if from > until {
            return Err(UnifiedError::bad_request("valid_from must not be after valid_until"));
        }
    }
    Ok(())
}

fn parse_optional_date(raw: &Option<String>, field: &str) -> UnifiedResult<Option<NaiveDate>> {
    match raw {
        Some(text) => Ok(Some(parse_date(text, field)?)),
        None => Ok(None),
    }
}

async fn list_staff_availability(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_VIEW)?;
        let staff_user_id = valid_user_id(&path.into_inner(), "staff_user_id")?;
        let params = query.into_inner();
        let mut items: Vec<StaffAvailability> = state
            .availability
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                row.staff_user_id == staff_user_id
                    && params
                        .filters
                        .get("kind")
                        .is_none_or(|value| row.kind == *value)
                    && params.filters.get("is_active").is_none_or(|value| {
                        row.is_active == (*value == "true" || *value == "1")
                    })
                    && params.filters.get("weekday").is_none_or(|value| {
                        row.weekday.map(|day| day.to_string()) == Some(value.clone())
                    })
            })
            .collect();
        items.sort_by_key(|row| (row.kind.clone(), row.weekday.unwrap_or(0), row.date));
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

async fn create_staff_availability(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateAvailabilityRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StaffAvailability> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let staff_user_id = valid_user_id(&path.into_inner(), "staff_user_id")?;
        let input = body.into_inner();
        let kind = input.kind.trim().to_string();
        let date = parse_optional_date(&input.date, "date")?;
        let valid_from = parse_optional_date(&input.valid_from, "valid_from")?;
        let valid_until = parse_optional_date(&input.valid_until, "valid_until")?;
        let reason = optional_text(&input.reason, "reason", MAX_REASON_LEN)?;
        validate_availability(
            &kind,
            input.weekday,
            date,
            input.start_minutes,
            input.end_minutes,
            input.break_start_minutes,
            input.break_end_minutes,
            valid_from,
            valid_until,
        )?;
        let now = Utc::now();
        let row = StaffAvailability {
            id: Uuid::now_v7(),
            staff_user_id: staff_user_id.clone(),
            kind,
            weekday: input.weekday,
            date,
            start_minutes: input.start_minutes,
            end_minutes: input.end_minutes,
            break_start_minutes: input.break_start_minutes,
            break_end_minutes: input.break_end_minutes,
            valid_from,
            valid_until,
            reason,
            is_active: input.is_active.unwrap_or(true),
            created_at: now,
            updated_at: now,
        };
        state.availability.insert(row.id, row.clone());
        state.emit(
            "appointments.availability.created",
            json!({ "availability_id": row.id, "staff_user_id": staff_user_id }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => created(row),
        Err(error) => err(error, debug),
    }
}

async fn update_staff_availability(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateAvailabilityRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StaffAvailability> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let (staff_user_id, availability_id) = path.into_inner();
        let staff_user_id = valid_user_id(&staff_user_id, "staff_user_id")?;
        let input = body.into_inner();
        let mut row = load_availability(&state, &staff_user_id, &availability_id)?;
        if let Some(kind) = &input.kind {
            row.kind = kind.trim().to_string();
        }
        if let Some(weekday) = input.weekday {
            row.weekday = weekday;
        }
        if let Some(date) = &input.date {
            row.date = match date {
                Some(text) => Some(parse_date(text, "date")?),
                None => None,
            };
        }
        if let Some(start) = input.start_minutes {
            row.start_minutes = start;
        }
        if let Some(end) = input.end_minutes {
            row.end_minutes = end;
        }
        if let Some(break_start) = input.break_start_minutes {
            row.break_start_minutes = break_start;
        }
        if let Some(break_end) = input.break_end_minutes {
            row.break_end_minutes = break_end;
        }
        if let Some(valid_from) = &input.valid_from {
            row.valid_from = match valid_from {
                Some(text) => Some(parse_date(text, "valid_from")?),
                None => None,
            };
        }
        if let Some(valid_until) = &input.valid_until {
            row.valid_until = match valid_until {
                Some(text) => Some(parse_date(text, "valid_until")?),
                None => None,
            };
        }
        if let Some(reason) = &input.reason {
            row.reason = optional_text(&Some(reason.clone()), "reason", MAX_REASON_LEN)?;
        }
        if let Some(is_active) = input.is_active {
            row.is_active = is_active;
        }
        validate_availability(
            &row.kind,
            row.weekday,
            row.date,
            row.start_minutes,
            row.end_minutes,
            row.break_start_minutes,
            row.break_end_minutes,
            row.valid_from,
            row.valid_until,
        )?;
        row.updated_at = Utc::now();
        state.availability.insert(row.id, row.clone());
        state.emit(
            "appointments.availability.updated",
            json!({ "availability_id": row.id, "staff_user_id": staff_user_id }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

async fn delete_staff_availability(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let (staff_user_id, availability_id) = path.into_inner();
        let staff_user_id = valid_user_id(&staff_user_id, "staff_user_id")?;
        let row = load_availability(&state, &staff_user_id, &availability_id)?;
        state.availability.remove(&row.id);
        state.emit(
            "appointments.availability.deleted",
            json!({ "availability_id": row.id, "staff_user_id": staff_user_id }),
        );
        Ok(json!({ "deleted": true, "id": row.id }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Handlers: staff-service assignments
// ---------------------------------------------------------------------------

async fn list_staff_services(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_VIEW)?;
        let staff_user_id = valid_user_id(&path.into_inner(), "staff_user_id")?;
        let params = query.into_inner();
        let mut items: Vec<StaffService> = state
            .staff_services
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                row.staff_user_id == staff_user_id
                    && params.filters.get("is_active").is_none_or(|value| {
                        row.is_active == (*value == "true" || *value == "1")
                    })
            })
            .collect();
        items.sort_by_key(|row| row.created_at);
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

async fn assign_staff_service(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AssignStaffServiceRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StaffService> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let staff_user_id = valid_user_id(&path.into_inner(), "staff_user_id")?;
        let service = load_service_by_id(&state, body.service_id)?;
        let existing = state.staff_services.iter().find(|entry| {
            let row = entry.value();
            row.staff_user_id == staff_user_id && row.service_id == service.id
        });
        if let Some(entry) = existing {
            let mut row = entry.value().clone();
            row.is_active = true;
            row.updated_at = Utc::now();
            state.staff_services.insert(row.id, row.clone());
            return Ok(row);
        }
        let now = Utc::now();
        let row = StaffService {
            id: Uuid::now_v7(),
            staff_user_id: staff_user_id.clone(),
            service_id: service.id,
            is_active: true,
            created_at: now,
            updated_at: now,
        };
        state.staff_services.insert(row.id, row.clone());
        state.emit(
            "appointments.staff.assigned",
            json!({ "staff_user_id": staff_user_id, "service_id": service.id }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => created(row),
        Err(error) => err(error, debug),
    }
}

async fn unassign_staff_service(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let (staff_user_id, service_id) = path.into_inner();
        let staff_user_id = valid_user_id(&staff_user_id, "staff_user_id")?;
        let service = load_service(&state, &service_id)?;
        let target = state.staff_services.iter().find(|entry| {
            let row = entry.value();
            row.staff_user_id == staff_user_id && row.service_id == service.id
        });
        match target {
            Some(entry) => {
                let id = *entry.key();
                state.staff_services.remove(&id);
                state.emit(
                    "appointments.staff.unassigned",
                    json!({ "staff_user_id": staff_user_id, "service_id": service.id }),
                );
                Ok(json!({ "deleted": true, "staff_user_id": staff_user_id, "service_id": service.id }))
            }
            None => Err(UnifiedError::not_found(
                "Staff member is not assigned to this service",
            )),
        }
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Handlers: staff calendar (JSON + iCalendar)
// ---------------------------------------------------------------------------

fn staff_appointments_in_range(
    state: &AppointmentsState,
    staff_user_id: &str,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Vec<Appointment> {
    let mut items: Vec<Appointment> = state
        .appointments
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|appointment| {
            appointment.staff_user_id.as_deref() == Some(staff_user_id)
                && from.is_none_or(|from| appointment.ends_at >= from)
                && to.is_none_or(|to| appointment.starts_at <= to)
        })
        .collect();
    items.sort_by_key(|appointment| appointment.starts_at);
    items
}

async fn staff_calendar(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<CalendarQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_VIEW)?;
        let staff_user_id = valid_user_id(&path.into_inner(), "staff_user_id")?;
        let input = query.into_inner();
        let from = match &input.from {
            Some(raw) => Some(parse_datetime(raw, "from")?),
            None => None,
        };
        let to = match &input.to {
            Some(raw) => Some(parse_datetime(raw, "to")?),
            None => None,
        };
        if let (Some(from), Some(to)) = (from, to) {
            if from > to {
                return Err(UnifiedError::bad_request("from must not be after to"));
            }
        }
        let items = staff_appointments_in_range(&state, &staff_user_id, from, to);
        Ok(json!({ "staff_user_id": staff_user_id, "appointments": items, "count": items.len() }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

async fn staff_calendar_ics(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<CalendarQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let staff_user_id = match valid_user_id(&path.into_inner(), "staff_user_id") {
        Ok(id) => id,
        Err(error) => return err(error, debug),
    };
    let caller_result = caller(&req).and_then(|caller| {
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_VIEW)?;
        Ok(())
    });
    if let Err(error) = caller_result {
        return err(error, debug);
    }
    let input = query.into_inner();
    let from = input.from.as_ref().and_then(|raw| parse_datetime(raw, "from").ok());
    let to = input.to.as_ref().and_then(|raw| parse_datetime(raw, "to").ok());
    let items = staff_appointments_in_range(&state, &staff_user_id, from, to);
    let mut blocks = Vec::new();
    for appointment in &items {
        if appointment.status == AppointmentStatus::Cancelled {
            continue;
        }
        if let Ok(service) = load_service_by_id(&state, appointment.service_id) {
            blocks.push(ics_appointment_block(appointment, &service));
        }
    }
    ics_response(build_ics(&blocks, &format!("Appointments — {staff_user_id}")))
}

// ---------------------------------------------------------------------------
// Handlers: appointments
// ---------------------------------------------------------------------------

/// Shared booking creation for the staff and guest flows. The end
/// time is always computed from the service duration; the caller
/// never supplies it.
#[allow(clippy::too_many_arguments)]
fn build_appointment(
    state: &AppointmentsState,
    service: &AppointmentService,
    staff_user_id: Option<String>,
    customer_user_id: Option<String>,
    customer_name: String,
    customer_email: String,
    customer_phone: Option<String>,
    starts_at: DateTime<Utc>,
    notes: Option<String>,
    source: &str,
    company_id: Option<String>,
) -> UnifiedResult<Appointment> {
    let settings = state.settings_snapshot();
    let now = Utc::now();
    validate_start_time(&settings, starts_at, now)?;
    let ends_at = starts_at + Duration::minutes(service.duration_minutes);
    if service.requires_staff {
        let staff = staff_user_id.as_deref().ok_or_else(|| {
            UnifiedError::bad_request("This service requires a staff member (staff_user_id)")
        })?;
        if !staff_assigned_to(state, staff, service.id) {
            return Err(UnifiedError::bad_request(
                "Staff member is not assigned to this service",
            ));
        }
    }
    if let Some(conflict) = find_conflict(
        state,
        service,
        staff_user_id.as_deref(),
        starts_at,
        ends_at,
        None,
    ) {
        return Err(UnifiedError::conflict(conflict));
    }
    let status = if settings.require_confirmation {
        AppointmentStatus::PendingConfirmation
    } else {
        AppointmentStatus::Confirmed
    };
    let appointment = Appointment {
        id: Uuid::now_v7(),
        service_id: service.id,
        staff_user_id,
        customer_user_id,
        customer_name,
        customer_email,
        customer_phone,
        starts_at,
        ends_at,
        status,
        source: source.to_string(),
        price: service.price,
        currency: service.currency.clone(),
        cancellation_fee_charged: None,
        cancelled_at: None,
        cancel_reason: None,
        notes,
        confirmation_code: confirmation_code(),
        company_id,
        created_at: now,
        updated_at: now,
    };
    state
        .appointments
        .insert(appointment.id, appointment.clone());
    state.emit(
        "appointments.created",
        json!({
            "appointment_id": appointment.id,
            "service_id": appointment.service_id,
            "staff_user_id": appointment.staff_user_id,
            "customer_user_id": appointment.customer_user_id,
            "starts_at": appointment.starts_at.to_rfc3339(),
            "status": appointment.status.as_str(),
            "source": appointment.source,
        }),
    );
    if status == AppointmentStatus::Confirmed {
        materialise_reminders(state, &appointment);
    }
    Ok(appointment)
}

/// Notify the customer and the staff member about a booking event.
/// `kind` is the notification kind, for example
/// `appointments.confirmed`.
fn notify_parties(state: &AppointmentsState, appointment: &Appointment, kind: &str, title: &str, body: &str) {
    let settings = state.settings_snapshot();
    let channels = settings.default_reminder_channels.clone();
    let (customer_id, customer_email) = AppointmentsState::customer_recipient(appointment);
    let link = format!(
        "/api/v1/appointments/public/bookings/{}",
        appointment.confirmation_code
    );
    state.notify(
        kind,
        &customer_id,
        Some(&customer_email),
        title,
        body,
        Some(link),
        &channels,
    );
    if let Some(staff) = &appointment.staff_user_id {
        state.notify(kind, staff, None, title, body, None, &channels);
    }
}

async fn list_appointments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_VIEW)?;
        let params = query.into_inner();
        let from = match params.filters.get("from") {
            Some(raw) => Some(parse_datetime(raw, "from")?),
            None => None,
        };
        let to = match params.filters.get("to") {
            Some(raw) => Some(parse_datetime(raw, "to")?),
            None => None,
        };
        let mut items: Vec<Appointment> = state
            .appointments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|appointment| {
                params.filters.get("service_id").is_none_or(|value| {
                    appointment.service_id.to_string() == *value
                }) && params.filters.get("staff_user_id").is_none_or(|value| {
                    appointment.staff_user_id.as_deref() == Some(value.as_str())
                }) && params.filters.get("customer_user_id").is_none_or(|value| {
                    appointment.customer_user_id.as_deref() == Some(value.as_str())
                }) && params.filters.get("status").is_none_or(|value| {
                    appointment.status.as_str() == value.as_str()
                }) && params.filters.get("source").is_none_or(|value| {
                    appointment.source == *value
                }) && params.filters.get("company_id").is_none_or(|value| {
                    appointment.company_id.as_deref() == Some(value.as_str())
                }) && from.is_none_or(|from| appointment.ends_at >= from)
                    && to.is_none_or(|to| appointment.starts_at <= to)
                    && params.search.as_ref().is_none_or(|needle| {
                        let needle = needle.to_lowercase();
                        appointment.customer_name.to_lowercase().contains(&needle)
                            || appointment.customer_email.to_lowercase().contains(&needle)
                    })
            })
            .collect();
        items.sort_by_key(|appointment| appointment.starts_at);
        if params.normalized().order == "desc" {
            items.reverse();
        }
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

async fn create_appointment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    body: web::Json<CreateAppointmentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Appointment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_CREATE)?;
        let input = body.into_inner();
        let service = load_service_by_id(&state, input.service_id)?;
        if !service.is_active {
            return Err(UnifiedError::not_found(format!("Service not found: {}", input.service_id)));
        }
        // Booking for another user needs the manage capability.
        let customer_user_id = match &input.customer_user_id {
            Some(other) if *other != caller.actor => {
                require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
                Some(valid_user_id(other, "customer_user_id")?)
            }
            Some(other) => Some(valid_user_id(other, "customer_user_id")?),
            None => Some(caller.actor.clone()),
        };
        let staff_user_id = match &input.staff_user_id {
            Some(staff) => Some(valid_user_id(staff, "staff_user_id")?),
            None => None,
        };
        let customer_name = require_text(&input.customer_name, "customer_name", MAX_NAME_LEN)?;
        let customer_email = valid_email(&input.customer_email)?;
        let customer_phone = optional_text(&input.customer_phone, "customer_phone", MAX_PHONE_LEN)?;
        let notes = optional_text(&input.notes, "notes", MAX_NOTES_LEN)?;
        let company_id = optional_text(&input.company_id, "company_id", MAX_USER_ID_LEN)?;
        let appointment = build_appointment(
            &state,
            &service,
            staff_user_id,
            customer_user_id,
            customer_name,
            customer_email,
            customer_phone,
            input.starts_at,
            notes,
            "customer",
            company_id,
        )?;
        let title = format!("Booking received: {}", service.name);
        let body = format!(
            "Your appointment \"{}\" is {} for {}. Confirmation code: {}.",
            service.name,
            appointment.status.as_str(),
            appointment.starts_at.to_rfc3339(),
            appointment.confirmation_code
        );
        notify_parties(&state, &appointment, "appointments.created", &title, &body);
        Ok(appointment)
    })();
    match result {
        Ok(appointment) => created(appointment),
        Err(error) => err(error, debug),
    }
}

async fn get_appointment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_VIEW)?;
        load_appointment(&state, &path.into_inner())
    })();
    match result {
        Ok(appointment) => ok(appointment),
        Err(error) => err(error, debug),
    }
}

async fn update_appointment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateAppointmentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Appointment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let input = body.into_inner();
        let mut appointment = load_appointment(&state, &path.into_inner())?;
        if matches!(
            appointment.status,
            AppointmentStatus::Cancelled | AppointmentStatus::Completed | AppointmentStatus::NoShow
        ) {
            return Err(UnifiedError::conflict(format!(
                "A {} appointment cannot be edited",
                appointment.status.as_str()
            )));
        }
        if let Some(name) = &input.customer_name {
            appointment.customer_name = require_text(name, "customer_name", MAX_NAME_LEN)?;
        }
        if let Some(email) = &input.customer_email {
            appointment.customer_email = valid_email(email)?;
        }
        if let Some(phone) = &input.customer_phone {
            appointment.customer_phone = optional_text(&Some(phone.clone()), "customer_phone", MAX_PHONE_LEN)?;
        }
        if let Some(notes) = &input.notes {
            appointment.notes = optional_text(&Some(notes.clone()), "notes", MAX_NOTES_LEN)?;
        }
        appointment.updated_at = Utc::now();
        state
            .appointments
            .insert(appointment.id, appointment.clone());
        Ok(appointment)
    })();
    match result {
        Ok(appointment) => ok(appointment),
        Err(error) => err(error, debug),
    }
}

async fn confirm_appointment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Appointment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let mut appointment = load_appointment(&state, &path.into_inner())?;
        if appointment.status != AppointmentStatus::PendingConfirmation {
            return Err(UnifiedError::conflict(format!(
                "Only a pending_confirmation appointment can be confirmed (current status: {})",
                appointment.status.as_str()
            )));
        }
        appointment.status = AppointmentStatus::Confirmed;
        appointment.updated_at = Utc::now();
        state
            .appointments
            .insert(appointment.id, appointment.clone());
        materialise_reminders(&state, &appointment);
        let service = load_service_by_id(&state, appointment.service_id)?;
        state.emit(
            "appointments.confirmed",
            json!({
                "appointment_id": appointment.id,
                "service_id": appointment.service_id,
                "starts_at": appointment.starts_at.to_rfc3339(),
            }),
        );
        let title = format!("Booking confirmed: {}", service.name);
        let body = format!(
            "Your appointment \"{}\" is confirmed for {}. Confirmation code: {}.",
            service.name,
            appointment.starts_at.to_rfc3339(),
            appointment.confirmation_code
        );
        notify_parties(&state, &appointment, "appointments.confirmed", &title, &body);
        Ok(appointment)
    })();
    match result {
        Ok(appointment) => ok(appointment),
        Err(error) => err(error, debug),
    }
}

async fn reschedule_appointment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RescheduleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Appointment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let input = body.into_inner();
        let mut appointment = load_appointment(&state, &path.into_inner())?;
        if !matches!(
            appointment.status,
            AppointmentStatus::PendingConfirmation | AppointmentStatus::Confirmed
        ) {
            return Err(UnifiedError::conflict(format!(
                "A {} appointment cannot be rescheduled",
                appointment.status.as_str()
            )));
        }
        let service = load_service_by_id(&state, appointment.service_id)?;
        if !service.is_active {
            return Err(UnifiedError::conflict("The service of this appointment is inactive"));
        }
        let settings = state.settings_snapshot();
        validate_start_time(&settings, input.starts_at, Utc::now())?;
        let ends_at = input.starts_at + Duration::minutes(service.duration_minutes);
        let staff_user_id = match &input.staff_user_id {
            Some(staff) => Some(valid_user_id(staff, "staff_user_id")?),
            None => appointment.staff_user_id.clone(),
        };
        if service.requires_staff {
            let staff = staff_user_id.as_deref().ok_or_else(|| {
                UnifiedError::bad_request("This service requires a staff member (staff_user_id)")
            })?;
            if !staff_assigned_to(&state, staff, service.id) {
                return Err(UnifiedError::bad_request(
                    "Staff member is not assigned to this service",
                ));
            }
        }
        if let Some(conflict) = find_conflict(
            &state,
            &service,
            staff_user_id.as_deref(),
            input.starts_at,
            ends_at,
            Some(appointment.id),
        ) {
            return Err(UnifiedError::conflict(conflict));
        }
        let previous_start = appointment.starts_at;
        appointment.starts_at = input.starts_at;
        appointment.ends_at = ends_at;
        appointment.staff_user_id = staff_user_id;
        appointment.updated_at = Utc::now();
        state
            .appointments
            .insert(appointment.id, appointment.clone());
        // Reminders belong to the old time: cancel them and build
        // fresh ones for the new time.
        cancel_pending_reminders(&state, appointment.id);
        if appointment.status == AppointmentStatus::Confirmed {
            materialise_reminders(&state, &appointment);
        }
        state.emit(
            "appointments.rescheduled",
            json!({
                "appointment_id": appointment.id,
                "previous_starts_at": previous_start.to_rfc3339(),
                "starts_at": appointment.starts_at.to_rfc3339(),
            }),
        );
        let title = format!("Appointment rescheduled: {}", service.name);
        let body = format!(
            "Your appointment \"{}\" moved from {} to {}. Confirmation code: {}.",
            service.name,
            previous_start.to_rfc3339(),
            appointment.starts_at.to_rfc3339(),
            appointment.confirmation_code
        );
        notify_parties(&state, &appointment, "appointments.rescheduled", &title, &body);
        Ok(appointment)
    })();
    match result {
        Ok(appointment) => ok(appointment),
        Err(error) => err(error, debug),
    }
}

async fn cancel_appointment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CancelRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Appointment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let input = body.into_inner();
        let mut appointment = load_appointment(&state, &path.into_inner())?;
        if !matches!(
            appointment.status,
            AppointmentStatus::PendingConfirmation | AppointmentStatus::Confirmed
        ) {
            return Err(UnifiedError::conflict(format!(
                "A {} appointment cannot be cancelled",
                appointment.status.as_str()
            )));
        }
        let service = load_service_by_id(&state, appointment.service_id)?;
        let settings = state.settings_snapshot();
        let now = Utc::now();
        let fee = cancellation_fee(&service, &settings, &appointment, now);
        let reason = optional_text(&input.reason, "reason", MAX_REASON_LEN)?;
        appointment.status = AppointmentStatus::Cancelled;
        appointment.cancellation_fee_charged = Some(fee);
        appointment.cancelled_at = Some(now);
        appointment.cancel_reason = reason;
        appointment.updated_at = now;
        state
            .appointments
            .insert(appointment.id, appointment.clone());
        cancel_pending_reminders(&state, appointment.id);
        state.emit(
            "appointments.cancelled",
            json!({
                "appointment_id": appointment.id,
                "service_id": appointment.service_id,
                "cancellation_fee_charged": fee,
                "currency": appointment.currency,
            }),
        );
        let title = format!("Appointment cancelled: {}", service.name);
        let body = if fee > 0.0 {
            format!(
                "Your appointment \"{}\" on {} was cancelled. A cancellation fee of {} {} applies.",
                service.name,
                appointment.starts_at.to_rfc3339(),
                fee,
                appointment.currency
            )
        } else {
            format!(
                "Your appointment \"{}\" on {} was cancelled. No fee applies.",
                service.name,
                appointment.starts_at.to_rfc3339()
            )
        };
        notify_parties(&state, &appointment, "appointments.cancelled", &title, &body);
        Ok(appointment)
    })();
    match result {
        Ok(appointment) => ok(appointment),
        Err(error) => err(error, debug),
    }
}

async fn complete_appointment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Appointment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let mut appointment = load_appointment(&state, &path.into_inner())?;
        if appointment.status != AppointmentStatus::Confirmed {
            return Err(UnifiedError::conflict(format!(
                "Only a confirmed appointment can be completed (current status: {})",
                appointment.status.as_str()
            )));
        }
        appointment.status = AppointmentStatus::Completed;
        appointment.updated_at = Utc::now();
        state
            .appointments
            .insert(appointment.id, appointment.clone());
        cancel_pending_reminders(&state, appointment.id);
        let service = load_service_by_id(&state, appointment.service_id)?;
        state.emit(
            "appointments.completed",
            json!({
                "appointment_id": appointment.id,
                "service_id": appointment.service_id,
                "staff_user_id": appointment.staff_user_id,
                "customer_user_id": appointment.customer_user_id,
                "customer_name": appointment.customer_name,
                "customer_email": appointment.customer_email,
                "price": appointment.price,
                "currency": appointment.currency,
                "starts_at": appointment.starts_at.to_rfc3339(),
                "ends_at": appointment.ends_at.to_rfc3339(),
            }),
        );
        // Integration contract for billing: request an invoice for
        // the completed appointment. No billing listener exists for
        // this event name today (checked during this build); the
        // billing module owns fulfilment once the integrator wires
        // a consumer, exactly like the events module's
        // `events.registration.refund_requested`.
        state.emit(
            "billing.invoice.requested",
            json!({
                "appointment_id": appointment.id,
                "service_id": appointment.service_id,
                "service_name": service.name,
                "customer_user_id": appointment.customer_user_id,
                "customer_name": appointment.customer_name,
                "customer_email": appointment.customer_email,
                "amount": appointment.price,
                "currency": appointment.currency,
                "description": format!("Appointment: {}", service.name),
            }),
        );
        // Integration contract for CRM: record the completed visit
        // as a customer activity. The CRM module owns fulfilment.
        state.emit(
            "crm.activity.requested",
            json!({
                "appointment_id": appointment.id,
                "customer_user_id": appointment.customer_user_id,
                "customer_name": appointment.customer_name,
                "customer_email": appointment.customer_email,
                "activity_type": "appointment_completed",
                "subject": format!("Completed appointment: {}", service.name),
                "starts_at": appointment.starts_at.to_rfc3339(),
            }),
        );
        Ok(appointment)
    })();
    match result {
        Ok(appointment) => ok(appointment),
        Err(error) => err(error, debug),
    }
}

async fn no_show_appointment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Appointment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let mut appointment = load_appointment(&state, &path.into_inner())?;
        if appointment.status != AppointmentStatus::Confirmed {
            return Err(UnifiedError::conflict(format!(
                "Only a confirmed appointment can be marked no-show (current status: {})",
                appointment.status.as_str()
            )));
        }
        appointment.status = AppointmentStatus::NoShow;
        appointment.updated_at = Utc::now();
        state
            .appointments
            .insert(appointment.id, appointment.clone());
        cancel_pending_reminders(&state, appointment.id);
        let service = load_service_by_id(&state, appointment.service_id)?;
        state.emit(
            "appointments.no_show",
            json!({
                "appointment_id": appointment.id,
                "service_id": appointment.service_id,
                "starts_at": appointment.starts_at.to_rfc3339(),
            }),
        );
        let title = format!("Missed appointment: {}", service.name);
        let body = format!(
            "You missed your appointment \"{}\" on {}. Please book again if you still need it.",
            service.name,
            appointment.starts_at.to_rfc3339()
        );
        notify_parties(&state, &appointment, "appointments.no_show", &title, &body);
        Ok(appointment)
    })();
    match result {
        Ok(appointment) => ok(appointment),
        Err(error) => err(error, debug),
    }
}

async fn appointment_ics(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller_result = caller(&req).and_then(|caller| {
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_VIEW)?;
        Ok(())
    });
    if let Err(error) = caller_result {
        return err(error, debug);
    }
    let result: UnifiedResult<String> = (|| {
        let appointment = load_appointment(&state, &path.into_inner())?;
        let service = load_service_by_id(&state, appointment.service_id)?;
        let block = ics_appointment_block(&appointment, &service);
        Ok(build_ics(&[block], &format!("Appointment — {}", service.name)))
    })();
    match result {
        Ok(body) => ics_response(body),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Handlers: public booking
// ---------------------------------------------------------------------------

async fn guest_book(
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    body: web::Json<GuestBookingRequest>,
) -> HttpResponse {
    // The generic failure below is used for every rejection in this
    // handler (honeypot, disabled guest booking, rate limit) so a
    // caller cannot tell the protections apart.
    const GENERIC_FAILURE: &str = "Booking could not be completed. Please try again later.";
    let settings = state.settings_snapshot();
    let key = client_key(&req);
    if check_guest_rate_limit(&state, &key, settings.guest_booking_rate_limit_per_hour).is_err() {
        return err(UnifiedError::new(GENERIC_FAILURE, 429), false);
    }
    let result: UnifiedResult<Appointment> = (|| {
        if !settings.allow_guest_booking {
            return Err(UnifiedError::new(GENERIC_FAILURE, 400));
        }
        let input = body.into_inner();
        if let Some(website) = &input.website {
            if !website.trim().is_empty() {
                return Err(UnifiedError::new(GENERIC_FAILURE, 400));
            }
        }
        let service = load_service_by_id(&state, input.service_id)
            .map_err(|_| UnifiedError::new(GENERIC_FAILURE, 400))?;
        if !service.is_active {
            return Err(UnifiedError::new(GENERIC_FAILURE, 400));
        }
        let staff_user_id = match &input.staff_user_id {
            Some(staff) => Some(valid_user_id(staff, "staff_user_id")?),
            None => None,
        };
        let customer_name = require_text(&input.customer_name, "customer_name", MAX_NAME_LEN)?;
        let customer_email = valid_email(&input.customer_email)?;
        let customer_phone = optional_text(&input.customer_phone, "customer_phone", MAX_PHONE_LEN)?;
        let notes = optional_text(&input.notes, "notes", MAX_NOTES_LEN)?;
        build_appointment(
            &state,
            &service,
            staff_user_id,
            None,
            customer_name,
            customer_email,
            customer_phone,
            input.starts_at,
            notes,
            "guest",
            None,
        )
    })();
    match result {
        Ok(appointment) => {
            let service = match load_service_by_id(&state, appointment.service_id) {
                Ok(service) => service,
                Err(error) => return err(error, false),
            };
            let title = format!("Booking received: {}", service.name);
            let body = format!(
                "Your appointment \"{}\" is {} for {}. Confirmation code: {}.",
                service.name,
                appointment.status.as_str(),
                appointment.starts_at.to_rfc3339(),
                appointment.confirmation_code
            );
            notify_parties(&state, &appointment, "appointments.created", &title, &body);
            created(public_appointment_view(&appointment, &service))
        }
        Err(error) => {
            // Map validation / conflict details to the generic
            // failure so spam probes learn nothing, except keep a
            // real conflict visible: a guest who picks a taken slot
            // needs to know to pick another time.
            if error.code == 409 {
                err(error, false)
            } else {
                err(UnifiedError::new(GENERIC_FAILURE, error.code), false)
            }
        }
    }
}

async fn public_booking_lookup(
    state: web::Data<AppointmentsState>,
    path: web::Path<String>,
) -> HttpResponse {
    let code = path.into_inner();
    let found = state
        .appointments
        .iter()
        .map(|entry| entry.value().clone())
        .find(|appointment| appointment.confirmation_code == code);
    match found {
        Some(appointment) => match load_service_by_id(&state, appointment.service_id) {
            Ok(service) => ok(public_appointment_view(&appointment, &service)),
            Err(error) => err(error, false),
        },
        None => err(UnifiedError::not_found("Booking not found"), false),
    }
}

// ---------------------------------------------------------------------------
// Handlers: appointment reminders
// ---------------------------------------------------------------------------

async fn list_appointment_reminders(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_VIEW)?;
        let appointment = load_appointment(&state, &path.into_inner())?;
        let params = query.into_inner();
        let mut items: Vec<AppointmentReminder> = state
            .reminders
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|reminder| {
                reminder.appointment_id == appointment.id
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|value| reminder.status == *value)
            })
            .collect();
        items.sort_by_key(|reminder| reminder.offset_minutes);
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

async fn create_appointment_reminder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateReminderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AppointmentReminder> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let appointment = load_appointment(&state, &path.into_inner())?;
        if matches!(
            appointment.status,
            AppointmentStatus::Cancelled | AppointmentStatus::Completed | AppointmentStatus::NoShow
        ) {
            return Err(UnifiedError::conflict(format!(
                "A {} appointment cannot gain new reminders",
                appointment.status.as_str()
            )));
        }
        let input = body.into_inner();
        if !(0..=MAX_OFFSET_MINUTES).contains(&input.offset_minutes) {
            return Err(UnifiedError::bad_request(format!(
                "offset_minutes must be between 0 and {MAX_OFFSET_MINUTES}"
            )));
        }
        let settings = state.settings_snapshot();
        let channels = if input.channels.is_empty() {
            settings.default_reminder_channels.clone()
        } else {
            valid_channels(&input.channels)?
        };
        let now = Utc::now();
        let mut reminder = AppointmentReminder {
            id: Uuid::now_v7(),
            appointment_id: appointment.id,
            offset_minutes: input.offset_minutes,
            channels,
            status: "pending".to_string(),
            scheduler_job_id: None,
            scheduler_status: "not_scheduled".to_string(),
            sent_at: None,
            created_at: now,
            updated_at: now,
        };
        schedule_reminder(&state, &mut reminder, &appointment);
        state.reminders.insert(reminder.id, reminder.clone());
        state.emit(
            "appointments.reminder.created",
            json!({
                "reminder_id": reminder.id,
                "appointment_id": appointment.id,
                "offset_minutes": reminder.offset_minutes,
            }),
        );
        Ok(reminder)
    })();
    match result {
        Ok(reminder) => created(reminder),
        Err(error) => err(error, debug),
    }
}

async fn delete_appointment_reminder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let (appointment_id, reminder_id) = path.into_inner();
        let appointment = load_appointment(&state, &appointment_id)?;
        let reminder = load_reminder(&state, &reminder_id)?;
        if reminder.appointment_id != appointment.id {
            return Err(UnifiedError::not_found(format!("Reminder not found: {reminder_id}")));
        }
        if reminder.status == "sent" {
            return Err(UnifiedError::conflict("A reminder that was already sent cannot be deleted"));
        }
        state.reminders.remove(&reminder.id);
        state.emit(
            "appointments.reminder.deleted",
            json!({ "reminder_id": reminder.id, "appointment_id": appointment.id }),
        );
        Ok(json!({ "deleted": true, "id": reminder.id }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

async fn send_appointment_reminder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let (appointment_id, reminder_id) = path.into_inner();
        let appointment = load_appointment(&state, &appointment_id)?;
        let reminder = load_reminder(&state, &reminder_id)?;
        if reminder.appointment_id != appointment.id {
            return Err(UnifiedError::not_found(format!("Reminder not found: {reminder_id}")));
        }
        if reminder.status == "sent" {
            return Err(UnifiedError::conflict("This reminder was already sent"));
        }
        let sent = send_reminder(&state, &reminder)?;
        let now = Utc::now();
        if let Some(mut row) = state.reminders.get_mut(&reminder.id) {
            row.value_mut().status = "sent".to_string();
            row.value_mut().sent_at = Some(now);
            row.value_mut().updated_at = now;
        }
        state.emit(
            "appointments.reminder.sent",
            json!({
                "reminder_id": reminder.id,
                "appointment_id": appointment.id,
                "sent": sent,
                "sent_at": now.to_rfc3339(),
            }),
        );
        Ok(json!({ "reminder_id": reminder.id, "sent": sent }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Handlers: reminder rules
// ---------------------------------------------------------------------------

async fn list_reminder_rules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<ReminderRule> = state
            .reminder_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| {
                params.filters.get("is_active").is_none_or(|value| {
                    rule.is_active == (*value == "true" || *value == "1")
                }) && params.filters.get("service_id").is_none_or(|value| {
                    rule.service_id.map(|id| id.to_string()) == Some(value.clone())
                })
            })
            .collect();
        items.sort_by_key(|rule| rule.offset_minutes);
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

async fn create_reminder_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    body: web::Json<CreateReminderRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ReminderRule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        if !(0..=MAX_OFFSET_MINUTES).contains(&input.offset_minutes) {
            return Err(UnifiedError::bad_request(format!(
                "offset_minutes must be between 0 and {MAX_OFFSET_MINUTES}"
            )));
        }
        let settings = state.settings_snapshot();
        let channels = if input.channels.is_empty() {
            settings.default_reminder_channels.clone()
        } else {
            valid_channels(&input.channels)?
        };
        if let Some(service_id) = input.service_id {
            load_service_by_id(&state, service_id)?;
        }
        let now = Utc::now();
        let rule = ReminderRule {
            id: Uuid::now_v7(),
            name,
            offset_minutes: input.offset_minutes,
            channels,
            service_id: input.service_id,
            is_active: input.is_active.unwrap_or(true),
            created_at: now,
            updated_at: now,
        };
        state.reminder_rules.insert(rule.id, rule.clone());
        state.emit(
            "appointments.reminder_rule.created",
            json!({ "rule_id": rule.id, "name": rule.name }),
        );
        Ok(rule)
    })();
    match result {
        Ok(rule) => created(rule),
        Err(error) => err(error, debug),
    }
}

async fn delete_reminder_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let rule = load_rule(&state, &path.into_inner())?;
        state.reminder_rules.remove(&rule.id);
        state.emit(
            "appointments.reminder_rule.deleted",
            json!({ "rule_id": rule.id, "name": rule.name }),
        );
        Ok(json!({ "deleted": true, "id": rule.id }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Handlers: settings
// ---------------------------------------------------------------------------

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AppointmentSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_VIEW)?;
        Ok(state.settings_snapshot())
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AppointmentSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.settings_snapshot();
        if let Some(slot) = input.default_slot_minutes {
            if !(MIN_SLOT_MINUTES..=MAX_SLOT_MINUTES).contains(&slot) {
                return Err(UnifiedError::bad_request(format!(
                    "default_slot_minutes must be between {MIN_SLOT_MINUTES} and {MAX_SLOT_MINUTES}"
                )));
            }
            settings.default_slot_minutes = slot;
        }
        for (value, field) in [
            (input.default_buffer_before_minutes, "default_buffer_before_minutes"),
            (input.default_buffer_after_minutes, "default_buffer_after_minutes"),
        ] {
            if let Some(minutes) = value {
                if !(0..=MAX_BUFFER_MINUTES).contains(&minutes) {
                    return Err(UnifiedError::bad_request(format!(
                        "{field} must be between 0 and {MAX_BUFFER_MINUTES}"
                    )));
                }
            }
        }
        if let Some(minutes) = input.default_buffer_before_minutes {
            settings.default_buffer_before_minutes = minutes;
        }
        if let Some(minutes) = input.default_buffer_after_minutes {
            settings.default_buffer_after_minutes = minutes;
        }
        if let Some(hours) = input.default_cancellation_notice_hours {
            if !(0..=MAX_NOTICE_HOURS).contains(&hours) {
                return Err(UnifiedError::bad_request(format!(
                    "default_cancellation_notice_hours must be between 0 and {MAX_NOTICE_HOURS}"
                )));
            }
            settings.default_cancellation_notice_hours = hours;
        }
        if let Some(percent) = input.default_cancellation_fee_percent {
            if !percent.is_finite() || !(0.0..=MAX_FEE_PERCENT).contains(&percent) {
                return Err(UnifiedError::bad_request(
                    "default_cancellation_fee_percent must be between 0 and 100",
                ));
            }
            settings.default_cancellation_fee_percent = percent;
        }
        if let Some(allow) = input.allow_guest_booking {
            settings.allow_guest_booking = allow;
        }
        if let Some(limit) = input.guest_booking_rate_limit_per_hour {
            if !(1..=MAX_RATE_LIMIT_PER_HOUR).contains(&limit) {
                return Err(UnifiedError::bad_request(format!(
                    "guest_booking_rate_limit_per_hour must be between 1 and {MAX_RATE_LIMIT_PER_HOUR}"
                )));
            }
            settings.guest_booking_rate_limit_per_hour = limit;
        }
        if let Some(days) = input.guest_booking_max_advance_days {
            if !(1..=MAX_ADVANCE_DAYS).contains(&days) {
                return Err(UnifiedError::bad_request(format!(
                    "guest_booking_max_advance_days must be between 1 and {MAX_ADVANCE_DAYS}"
                )));
            }
            settings.guest_booking_max_advance_days = days;
        }
        if let Some(minutes) = input.min_advance_booking_minutes {
            if !(0..=MAX_OFFSET_MINUTES).contains(&minutes) {
                return Err(UnifiedError::bad_request(
                    "min_advance_booking_minutes must be between 0 and 43200",
                ));
            }
            settings.min_advance_booking_minutes = minutes;
        }
        if let Some(channels) = input.default_reminder_channels {
            settings.default_reminder_channels = valid_channels(&channels)?;
        }
        if let Some(types) = input.allowed_location_types {
            if types.is_empty() {
                return Err(UnifiedError::bad_request(
                    "allowed_location_types must not be empty",
                ));
            }
            let mut cleaned = Vec::with_capacity(types.len());
            for location_type in types {
                let trimmed = location_type.trim().to_string();
                if trimmed.is_empty() || trimmed.len() > MAX_LOCATION_LEN {
                    return Err(UnifiedError::bad_request(
                        "allowed_location_types entries must be non-empty and at most 200 characters",
                    ));
                }
                if !cleaned.contains(&trimmed) {
                    cleaned.push(trimmed);
                }
            }
            settings.allowed_location_types = cleaned;
        }
        if let Some(require) = input.require_confirmation {
            settings.require_confirmation = require;
        }
        settings.updated_at = Utc::now();
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        state.emit(
            "appointments.settings.updated",
            json!({ "updated_at": settings.updated_at.to_rfc3339() }),
        );
        Ok(settings)
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct AppointmentsModule {
    state: AppointmentsState,
}

impl AppointmentsModule {
    pub fn new() -> Self {
        Self {
            state: AppointmentsState::new(),
        }
    }
}

impl Default for AppointmentsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for AppointmentsModule {
    fn id(&self) -> &'static str {
        "appointments"
    }

    fn name(&self) -> &'static str {
        "Appointments"
    }

    fn description(&self) -> &'static str {
        "Appointment booking: bookable services with duration, price, capacity and buffers; staff calendars with working hours, breaks and days off; slot generation; bookings with server-side double-booking prevention, reschedule and cancel with a configurable cancellation policy, no-show marking, reminder rules through notifications, guest booking with honeypot and rate limiting, and iCalendar exports. Completed appointments emit billing and CRM integration events."
    }

    fn requires(&self) -> &[&'static str] {
        &["events", "users", "notifications", "billing"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["events", "users", "notifications", "billing"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
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
        let state = self.state.clone();
        bus.register(
            REMINDERS_SEND_REQUESTED,
            "appointments",
            Arc::new(move |ctx: &mut HookContext| {
                on_reminders_send_requested(state.clone(), ctx)
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/appointments/public/services", owner: "appointments", description: "PUBLIC: list active services (no auth; gated by the API Manager ApiAccessRule for this path)." },
            RouteInfo { method: "GET", path: "/api/v1/appointments/public/services/{id}/slots", owner: "appointments", description: "PUBLIC: bookable slots for a service on one day (no auth; gated by the API Manager ApiAccessRule)." },
            RouteInfo { method: "POST", path: "/api/v1/appointments/public/book", owner: "appointments", description: "PUBLIC: guest booking with honeypot and rate limit (no auth; gated by the API Manager ApiAccessRule)." },
            RouteInfo { method: "GET", path: "/api/v1/appointments/public/bookings/{code}", owner: "appointments", description: "PUBLIC: look a booking up by confirmation code (no auth; gated by the API Manager ApiAccessRule)." },
            RouteInfo { method: "GET", path: "/api/v1/appointments/services", owner: "appointments", description: "List services." },
            RouteInfo { method: "POST", path: "/api/v1/appointments/services", owner: "appointments", description: "Create a service." },
            RouteInfo { method: "GET", path: "/api/v1/appointments/services/{id}", owner: "appointments", description: "Get one service." },
            RouteInfo { method: "PATCH", path: "/api/v1/appointments/services/{id}", owner: "appointments", description: "Update a service." },
            RouteInfo { method: "DELETE", path: "/api/v1/appointments/services/{id}", owner: "appointments", description: "Delete a service with no active bookings." },
            RouteInfo { method: "GET", path: "/api/v1/appointments/services/{id}/slots", owner: "appointments", description: "Bookable slots for a service on one day." },
            RouteInfo { method: "GET", path: "/api/v1/appointments/staff/{staff_id}/availability", owner: "appointments", description: "List a staff member's availability (working hours, breaks, days off)." },
            RouteInfo { method: "POST", path: "/api/v1/appointments/staff/{staff_id}/availability", owner: "appointments", description: "Create working hours or a day off for a staff member." },
            RouteInfo { method: "PATCH", path: "/api/v1/appointments/staff/{staff_id}/availability/{availability_id}", owner: "appointments", description: "Update one availability row." },
            RouteInfo { method: "DELETE", path: "/api/v1/appointments/staff/{staff_id}/availability/{availability_id}", owner: "appointments", description: "Delete one availability row." },
            RouteInfo { method: "GET", path: "/api/v1/appointments/staff/{staff_id}/services", owner: "appointments", description: "List services a staff member is assigned to." },
            RouteInfo { method: "POST", path: "/api/v1/appointments/staff/{staff_id}/services", owner: "appointments", description: "Assign a staff member to a service." },
            RouteInfo { method: "DELETE", path: "/api/v1/appointments/staff/{staff_id}/services/{service_id}", owner: "appointments", description: "Unassign a staff member from a service." },
            RouteInfo { method: "GET", path: "/api/v1/appointments/staff/{staff_id}/calendar", owner: "appointments", description: "A staff member's appointments in a date range." },
            RouteInfo { method: "GET", path: "/api/v1/appointments/staff/{staff_id}/calendar.ics", owner: "appointments", description: "A staff member's appointments as an RFC 5545 .ics feed." },
            RouteInfo { method: "GET", path: "/api/v1/appointments/reminder-rules", owner: "appointments", description: "List configurable reminder rules." },
            RouteInfo { method: "POST", path: "/api/v1/appointments/reminder-rules", owner: "appointments", description: "Create a reminder rule." },
            RouteInfo { method: "DELETE", path: "/api/v1/appointments/reminder-rules/{rule_id}", owner: "appointments", description: "Delete a reminder rule." },
            RouteInfo { method: "GET", path: "/api/v1/appointments/settings", owner: "appointments", description: "Get module settings (slot length, buffers, cancellation policy, guest booking)." },
            RouteInfo { method: "PATCH", path: "/api/v1/appointments/settings", owner: "appointments", description: "Update module settings." },
            RouteInfo { method: "GET", path: "/api/v1/appointments", owner: "appointments", description: "List appointments (paginated, filter by service, staff, customer, status, date range)." },
            RouteInfo { method: "POST", path: "/api/v1/appointments", owner: "appointments", description: "Create a booking (server-side double-booking check)." },
            RouteInfo { method: "GET", path: "/api/v1/appointments/{id}", owner: "appointments", description: "Get one appointment." },
            RouteInfo { method: "PATCH", path: "/api/v1/appointments/{id}", owner: "appointments", description: "Update customer details / notes of an appointment." },
            RouteInfo { method: "POST", path: "/api/v1/appointments/{id}/confirm", owner: "appointments", description: "Confirm a pending appointment and materialise its reminders." },
            RouteInfo { method: "POST", path: "/api/v1/appointments/{id}/reschedule", owner: "appointments", description: "Move an appointment to a new slot (conflict-checked, reminders rebuilt)." },
            RouteInfo { method: "POST", path: "/api/v1/appointments/{id}/cancel", owner: "appointments", description: "Cancel an appointment (cancellation policy fee applied when inside the notice window)." },
            RouteInfo { method: "POST", path: "/api/v1/appointments/{id}/complete", owner: "appointments", description: "Complete an appointment (emits billing.invoice.requested and crm.activity.requested)." },
            RouteInfo { method: "POST", path: "/api/v1/appointments/{id}/no-show", owner: "appointments", description: "Mark a confirmed appointment as no-show." },
            RouteInfo { method: "GET", path: "/api/v1/appointments/{id}/ics", owner: "appointments", description: "One appointment as an RFC 5545 .ics file." },
            RouteInfo { method: "GET", path: "/api/v1/appointments/{id}/reminders", owner: "appointments", description: "List reminders of an appointment." },
            RouteInfo { method: "POST", path: "/api/v1/appointments/{id}/reminders", owner: "appointments", description: "Create a reminder for an appointment (queued as appointments.reminders.send)." },
            RouteInfo { method: "DELETE", path: "/api/v1/appointments/{id}/reminders/{reminder_id}", owner: "appointments", description: "Delete a pending reminder." },
            RouteInfo { method: "POST", path: "/api/v1/appointments/{id}/reminders/{reminder_id}/send", owner: "appointments", description: "Send a reminder now through notification.requested." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/appointments")
                .app_data(state)
                // Public routes first: static segments must win over
                // the `/{id}` routes below.
                .route("/public/services", web::get().to(list_public_services))
                .route("/public/services/{id}/slots", web::get().to(list_public_service_slots))
                .route("/public/book", web::post().to(guest_book))
                .route("/public/bookings/{code}", web::get().to(public_booking_lookup))
                // Services.
                .route("/services", web::get().to(list_services))
                .route("/services", web::post().to(create_service))
                .route("/services/{id}", web::get().to(get_service))
                .route("/services/{id}", web::patch().to(update_service))
                .route("/services/{id}", web::delete().to(delete_service))
                .route("/services/{id}/slots", web::get().to(list_service_slots))
                // Staff calendars.
                .route("/staff/{staff_id}/availability", web::get().to(list_staff_availability))
                .route("/staff/{staff_id}/availability", web::post().to(create_staff_availability))
                .route(
                    "/staff/{staff_id}/availability/{availability_id}",
                    web::patch().to(update_staff_availability),
                )
                .route(
                    "/staff/{staff_id}/availability/{availability_id}",
                    web::delete().to(delete_staff_availability),
                )
                .route("/staff/{staff_id}/services", web::get().to(list_staff_services))
                .route("/staff/{staff_id}/services", web::post().to(assign_staff_service))
                .route(
                    "/staff/{staff_id}/services/{service_id}",
                    web::delete().to(unassign_staff_service),
                )
                .route("/staff/{staff_id}/calendar", web::get().to(staff_calendar))
                .route("/staff/{staff_id}/calendar.ics", web::get().to(staff_calendar_ics))
                // Reminder rules and settings (static segments, before `/{id}`).
                .route("/reminder-rules", web::get().to(list_reminder_rules))
                .route("/reminder-rules", web::post().to(create_reminder_rule))
                .route("/reminder-rules/{rule_id}", web::delete().to(delete_reminder_rule))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings))
                // Appointments.
                .route("", web::get().to(list_appointments))
                .route("", web::post().to(create_appointment))
                .route("/{id}", web::get().to(get_appointment))
                .route("/{id}", web::patch().to(update_appointment))
                .route("/{id}/confirm", web::post().to(confirm_appointment))
                .route("/{id}/reschedule", web::post().to(reschedule_appointment))
                .route("/{id}/cancel", web::post().to(cancel_appointment))
                .route("/{id}/complete", web::post().to(complete_appointment))
                .route("/{id}/no-show", web::post().to(no_show_appointment))
                .route("/{id}/ics", web::get().to(appointment_ics))
                .route("/{id}/reminders", web::get().to(list_appointment_reminders))
                .route("/{id}/reminders", web::post().to(create_appointment_reminder))
                .route(
                    "/{id}/reminders/{reminder_id}",
                    web::delete().to(delete_appointment_reminder),
                )
                .route(
                    "/{id}/reminders/{reminder_id}/send",
                    web::post().to(send_appointment_reminder),
                ),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AppointmentsModule::new()));
}
