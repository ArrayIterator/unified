// admissions module — Module constants.
//
// Split from the former single-file module `src/modules/admissions.rs`.
// The module root (`admissions.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Capabilities (checked through the `role.check` hook; fail closed).
// ---------------------------------------------------------------------------

/// Read admission records (applicants see only their own).
pub const PERM_ADMISSIONS_READ: &str = "admissions:read";
/// Read every application and result.
pub const PERM_ADMISSIONS_READ_ALL: &str = "admissions:read_all";
/// Manage periods, quotas, stages, required documents, decisions,
/// checklist verification, fee waivers, and result publication.
pub const PERM_ADMISSIONS_MANAGE: &str = "admissions:manage";
/// File and edit your own application.
pub const PERM_ADMISSIONS_APPLY: &str = "admissions:apply";
/// Record and correct stage scores.
pub const PERM_ADMISSIONS_SCORE: &str = "admissions:score";

/// Longest free-text field accepted (names, notes, reasons).
pub(crate) const MAX_TEXT: usize = 2000;
/// Longest short field accepted (titles, codes, keys, slugs).
pub(crate) const MAX_SHORT: usize = 255;
/// Longest identifier accepted (user ids, program ids, form ids).
pub(crate) const MAX_ID: usize = 128;
/// Largest money amount accepted in a request. NUMERIC(19,4) holds at
/// most 15 integer digits, so anything at or above 1e15 is rejected.
pub(crate) const MAX_MONEY: f64 = 1_000_000_000_000_000.0;
/// Longest settings key/value accepted.
pub(crate) const MAX_SETTING_KEY: usize = 128;
pub(crate) const MAX_SETTING_VALUE: usize = 2000;

/// Seed defaults for the data-driven vocabularies and defaults.
/// These are CONFIGURABLE DEFAULTS: they seed the settings table on
/// first use and an administrator may change them at any time
/// through `PUT /api/v1/admissions/settings`. Nothing in the handler
/// logic treats these strings as fixed law except the transition
/// rules documented on the handlers that use them.
pub(crate) const SEED_PERIOD_STATUSES: &str = "draft,open,closed,archived";
pub(crate) const SEED_APPLICATION_STATUSES: &str =
    "draft,submitted,under_review,accepted,rejected,waitlisted,enrolled,withdrawn";
pub(crate) const SEED_CHECKLIST_STATUSES: &str = "pending,submitted,verified,rejected";
pub(crate) const SEED_WAITLIST_STATUSES: &str = "waiting,promoted,declined,expired";
pub(crate) const SEED_DECISIONS: &str = "accepted,rejected,waitlisted";
pub(crate) const SEED_FEE_CURRENCY: &str = "USD";
pub(crate) const SEED_SCORE_SCALE_MAX: &str = "100";
pub(crate) const SEED_WAITLIST_ORDER: &str = "score_desc_submitted_asc";
pub(crate) const SEED_RESULT_VISIBLE: &str = "true";

/// Settings keys for the vocabularies and defaults above.
pub(crate) const KEY_PERIOD_STATUSES: &str = "status.period";
pub(crate) const KEY_APPLICATION_STATUSES: &str = "status.application";
pub(crate) const KEY_CHECKLIST_STATUSES: &str = "status.checklist";
pub(crate) const KEY_WAITLIST_STATUSES: &str = "status.waitlist";
pub(crate) const KEY_DECISIONS: &str = "decision.result";
pub(crate) const KEY_FEE_CURRENCY: &str = "defaults.fee_currency";
pub(crate) const KEY_SCORE_SCALE_MAX: &str = "defaults.score_scale_max";
pub(crate) const KEY_WAITLIST_ORDER: &str = "defaults.waitlist_order";
pub(crate) const KEY_RESULT_VISIBLE: &str = "defaults.result_visible_to_applicant";

/// Waitlist ordering policies understood by the ordering helper.
/// The policy itself is data (the `defaults.waitlist_order`
/// setting); only the small set of policy names is fixed here.
pub(crate) const WAITLIST_ORDERS: [&str; 2] = ["score_desc_submitted_asc", "submitted_asc"];

/// Fee invoice states, tracked on the application itself.
pub(crate) const FEE_STATES: [&str; 4] = ["none", "requested", "paid", "waived"];
