// Events Manager — Event names, permission strings, limits, and other constants.

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
use super::module::*;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_TITLE_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_SLUG_LEN: usize = 128;

pub(crate) const MAX_CATEGORY_LEN: usize = 80;

pub(crate) const MAX_TAG_LEN: usize = 64;

pub(crate) const MAX_TAGS: usize = 20;

pub(crate) const MAX_VENUE_NAME_LEN: usize = 200;

pub(crate) const MAX_ADDRESS_LEN: usize = 500;

pub(crate) const MAX_TIMEZONE_LEN: usize = 64;

pub(crate) const MAX_COVER_KEY_LEN: usize = 512;

pub(crate) const MAX_NAME_LEN: usize = 200;

pub(crate) const MAX_EMAIL_LEN: usize = 254;

pub(crate) const MAX_CHECKIN_CODE_LEN: usize = 64;

/// Hard cap on materialised / expanded occurrences per event, so a
/// runaway recurrence rule can never allocate unbounded rows.
pub(crate) const MAX_OCCURRENCES: usize = 366;

/// Default reminder offsets (minutes before the event start).
pub(crate) const DEFAULT_REMINDER_OFFSETS_MIN: [i64; 2] = [1440, 60];

/// Maximum ticket quantity and per-order limit.
pub(crate) const MAX_TICKET_QUANTITY: i64 = 100_000;

pub(crate) const MAX_PER_ORDER: i64 = 100;

/// Default per-order ticket limit when the request omits one.
pub(crate) const DEFAULT_PER_ORDER: i64 = 10;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_EVENTS_VIEW: &str = "events.view";

pub(crate) const PERM_EVENTS_CREATE: &str = "events.create";

pub(crate) const PERM_EVENTS_MANAGE: &str = "events.manage";

pub(crate) const PERM_EVENTS_CHECKIN: &str = "events.checkin";

/// Hook event names this module listens to (literals repeated here
/// so the module stays decoupled from the payments / store modules,
/// exactly like the store module repeats the payments constants).
pub(crate) const PAYMENT_CAPTURED: &str = "payment.captured";

pub(crate) const BANK_TRANSFER_VERIFICATION_CREATED: &str = "bank_transfer.verification.created";

pub(crate) const BANK_TRANSFER_REJECTED: &str = "bank_transfer.rejected";

pub(crate) const REMINDERS_SEND_REQUESTED: &str = "events.reminders.send_requested";

// Re-exported for the integrator's tests: the scheduler job kind
// this module enqueues, and the hook event the scheduler must emit
// when that job fires.
pub const EVENTS_REMINDERS_JOB_KIND: &str = "events.reminders.send";

pub const EVENTS_REMINDERS_SEND_REQUESTED: &str = REMINDERS_SEND_REQUESTED;
