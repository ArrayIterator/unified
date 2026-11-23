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


use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
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
use super::ai_capabilities::AI_CAPABILITIES;

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

        ai::register_ai_capabilities("events", AI_CAPABILITIES);
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
            RouteInfo { method: "GET", path: "/api/v1/events", owner: "events", description: "List events (paginated, filter by status, visibility, category, organizer_id, from, to).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/events", owner: "events", description: "Create an event (draft; default 24h/1h reminders are created).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/events/{id}", owner: "events", description: "Get one event.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/events/{id}", owner: "events", description: "Update a draft or published event (organizer or manage capability).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/events/{id}", owner: "events", description: "Delete a draft or cancelled event.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/events/{id}/publish", owner: "events", description: "Publish a draft event.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/events/{id}/cancel", owner: "events", description: "Cancel an event.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/events/{id}/complete", owner: "events", description: "Mark an event completed.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/events/{id}/occurrences", owner: "events", description: "List materialised occurrences of an event (recurrence expanded).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/events/{id}/occurrences/{occurrence_id}/cancel", owner: "events", description: "Cancel one occurrence of a recurring event.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/events/{id}/occurrences/{occurrence_id}/override", owner: "events", description: "Override one occurrence's time and/or venue (or cancel it).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/events/{id}/ticket-types", owner: "events", description: "List ticket types of an event.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/events/{id}/ticket-types", owner: "events", description: "Create a ticket type for an event.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/events/{id}/ticket-types/{ticket_type_id}", owner: "events", description: "Get one ticket type.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/events/{id}/ticket-types/{ticket_type_id}", owner: "events", description: "Update a ticket type (not after sales started).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/events/{id}/ticket-types/{ticket_type_id}", owner: "events", description: "Delete a ticket type with no sales.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/events/{id}/register", owner: "events", description: "Register for an event (self-registration; free confirms at once, paid waits for payment.captured; sold out joins the waitlist).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/events/{id}/registrations", owner: "events", description: "List registrations of an event (paginated, filter by status, ticket_type_id, user_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/events/registrations/{registration_id}", owner: "events", description: "Get one registration (owner or manage capability).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/events/registrations/{registration_id}/cancel", owner: "events", description: "Cancel a registration (owner or manage capability); freed seats promote the waitlist.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/events/registrations/{registration_id}/refund-request", owner: "events", description: "Request a refund (emits events.registration.refund_requested for the billing/store side; no refund is processed here).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/events/registrations/{registration_id}/check-in", owner: "events", description: "Check in by check-in code, or by id for an organizer (check-in capability).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/events/{id}/attendees", owner: "events", description: "List attendees of an event (paginated, filter by checked_in, registration_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/events/{id}/attendees/export", owner: "events", description: "Export attendees as CSV or JSON (?format=csv|json).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/events/{id}/waitlist", owner: "events", description: "List waitlist entries of an event (arrival order).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/events/{id}/reminders", owner: "events", description: "List reminders of an event.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/events/{id}/reminders", owner: "events", description: "Create a reminder (offset in minutes before start; queued as events.reminders.send).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/events/{id}/reminders/{reminder_id}", owner: "events", description: "Delete a pending reminder.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/events/{id}/reminders/{reminder_id}/send", owner: "events", description: "Send a reminder now through notification.requested.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/events/{id}/ics", owner: "events", description: "One event as an RFC 5545 .ics file (recurrence included).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/events/calendar.ics", owner: "events", description: "Calendar feed (.ics) of all non-cancelled events.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/events/calendar.json", owner: "events", description: "Calendar feed (JSON) of all non-cancelled events.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/events/public", owner: "events", description: "PUBLIC: list published, public events (no auth; gated by the API Manager ApiAccessRule for this path).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/events/public/{slug}", owner: "events", description: "PUBLIC: one published event with ticket types and available seats (no auth; gated by the API Manager ApiAccessRule).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/events/public/{slug}/ics", owner: "events", description: "PUBLIC: one published event as .ics (no auth; gated by the API Manager ApiAccessRule).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/events/public/calendar.ics", owner: "events", description: "PUBLIC: calendar feed (.ics) of published, public events (no auth; gated by the API Manager ApiAccessRule).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/events/public/calendar.json", owner: "events", description: "PUBLIC: calendar feed (JSON) of published, public events (no auth; gated by the API Manager ApiAccessRule).", auth: unified_core::auth::RouteAuth::Public},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(EventsModule::new()));
}
