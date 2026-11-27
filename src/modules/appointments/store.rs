// appointments - store.

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
use super::types::*;
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

// Shared state

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

    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    pub(crate) fn settings_snapshot(&self) -> AppointmentSettings {
        self.settings.read().expect("settings lock poisoned").clone()
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape: `kind`, `user_id`, `title`, `body`,
    /// `module: "appointments"`, `link`, `priority`, `channels`.
    /// For guest bookings `user_id` is the customer email address
    /// and the same address is passed as `email` so the email
    /// channel can deliver it.
    pub(crate) fn notify(
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
    pub(crate) fn customer_recipient(appointment: &Appointment) -> (String, String) {
        match &appointment.customer_user_id {
            Some(user_id) => (user_id.clone(), appointment.customer_email.clone()),
            None => (
                appointment.customer_email.clone(),
                appointment.customer_email.clone(),
            ),
        }
    }
}
