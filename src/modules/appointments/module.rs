// appointments — module.

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
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
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

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct AppointmentsModule {
    pub(crate) state: AppointmentsState,
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

        ai::register_ai_capabilities("appointments", AI_CAPABILITIES);
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
            RouteInfo { method: "GET", path: "/api/v1/appointments/public/services", owner: "appointments", description: "PUBLIC: list active services (no auth; gated by the API Manager ApiAccessRule for this path).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/appointments/public/services/{id}/slots", owner: "appointments", description: "PUBLIC: bookable slots for a service on one day (no auth; gated by the API Manager ApiAccessRule).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/appointments/public/book", owner: "appointments", description: "PUBLIC: guest booking with honeypot and rate limit (no auth; gated by the API Manager ApiAccessRule).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/appointments/public/bookings/{code}", owner: "appointments", description: "PUBLIC: look a booking up by confirmation code (no auth; gated by the API Manager ApiAccessRule).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/appointments/services", owner: "appointments", description: "List services.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/appointments/services", owner: "appointments", description: "Create a service.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/appointments/services/{id}", owner: "appointments", description: "Get one service.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/appointments/services/{id}", owner: "appointments", description: "Update a service.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/appointments/services/{id}", owner: "appointments", description: "Delete a service with no active bookings.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/appointments/services/{id}/slots", owner: "appointments", description: "Bookable slots for a service on one day.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/appointments/staff/{staff_id}/availability", owner: "appointments", description: "List a staff member's availability (working hours, breaks, days off).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/appointments/staff/{staff_id}/availability", owner: "appointments", description: "Create working hours or a day off for a staff member.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/appointments/staff/{staff_id}/availability/{availability_id}", owner: "appointments", description: "Update one availability row.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/appointments/staff/{staff_id}/availability/{availability_id}", owner: "appointments", description: "Delete one availability row.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/appointments/staff/{staff_id}/services", owner: "appointments", description: "List services a staff member is assigned to.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/appointments/staff/{staff_id}/services", owner: "appointments", description: "Assign a staff member to a service.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/appointments/staff/{staff_id}/services/{service_id}", owner: "appointments", description: "Unassign a staff member from a service.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/appointments/staff/{staff_id}/calendar", owner: "appointments", description: "A staff member's appointments in a date range.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/appointments/staff/{staff_id}/calendar.ics", owner: "appointments", description: "A staff member's appointments as an RFC 5545 .ics feed.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/appointments/reminder-rules", owner: "appointments", description: "List configurable reminder rules.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/appointments/reminder-rules", owner: "appointments", description: "Create a reminder rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/appointments/reminder-rules/{rule_id}", owner: "appointments", description: "Delete a reminder rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/appointments/settings", owner: "appointments", description: "Get module settings (slot length, buffers, cancellation policy, guest booking).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/appointments/settings", owner: "appointments", description: "Update module settings.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/appointments", owner: "appointments", description: "List appointments (paginated, filter by service, staff, customer, status, date range).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/appointments", owner: "appointments", description: "Create a booking (server-side double-booking check).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/appointments/{id}", owner: "appointments", description: "Get one appointment.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/appointments/{id}", owner: "appointments", description: "Update customer details / notes of an appointment.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/appointments/{id}/confirm", owner: "appointments", description: "Confirm a pending appointment and materialise its reminders.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/appointments/{id}/reschedule", owner: "appointments", description: "Move an appointment to a new slot (conflict-checked, reminders rebuilt).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/appointments/{id}/cancel", owner: "appointments", description: "Cancel an appointment (cancellation policy fee applied when inside the notice window).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/appointments/{id}/complete", owner: "appointments", description: "Complete an appointment (emits billing.invoice.requested and crm.activity.requested).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/appointments/{id}/no-show", owner: "appointments", description: "Mark a confirmed appointment as no-show.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/appointments/{id}/ics", owner: "appointments", description: "One appointment as an RFC 5545 .ics file.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/appointments/{id}/reminders", owner: "appointments", description: "List reminders of an appointment.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/appointments/{id}/reminders", owner: "appointments", description: "Create a reminder for an appointment (queued as appointments.reminders.send).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/appointments/{id}/reminders/{reminder_id}", owner: "appointments", description: "Delete a pending reminder.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/appointments/{id}/reminders/{reminder_id}/send", owner: "appointments", description: "Send a reminder now through notification.requested.", auth: unified_core::auth::RouteAuth::Authenticated},
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
