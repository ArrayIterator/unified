// Events Manager - In-memory state and its constructors.

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
use super::types::*;
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

// Shared state

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
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
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
    pub(crate) fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
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
