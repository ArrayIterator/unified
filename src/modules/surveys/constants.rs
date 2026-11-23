// surveys module — Module constants.
//
// Split from the former single-file module `src/modules/surveys.rs`.
// The module root (`surveys.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine;
use chrono::{DateTime, Datelike, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_TITLE_LEN: usize = 200;
pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;
pub(crate) const MAX_QUESTION_LEN: usize = 1000;
pub(crate) const MAX_TEXT_LEN: usize = 5000;
pub(crate) const MAX_ID_LEN: usize = 255;
pub(crate) const MAX_EMAIL_LEN: usize = 254;
pub(crate) const MAX_OPTIONS: usize = 20;
pub(crate) const MAX_OPTION_LEN: usize = 200;
pub(crate) const MAX_RECIPIENTS: usize = 1000;
/// Default token lifetime for a response invitation (days).
pub(crate) const DEFAULT_TOKEN_DAYS: i64 = 30;
/// NPS detractor threshold: NPS answers 0..=6 are detractors,
/// 7–8 passives, 9–10 promoters.
pub(crate) const NPS_DETRACTOR_MAX: i32 = 6;
pub(crate) const NPS_PROMOTER_MIN: i32 = 9;
/// CSAT satisfied threshold: scores 4–5 of 5 count as satisfied;
/// 1–2 count as dissatisfied and trigger a follow-up.
pub(crate) const CSAT_SATISFIED_MIN: i32 = 4;
pub(crate) const CSAT_FOLLOWUP_MAX: i32 = 2;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_SURVEYS_VIEW: &str = "surveys.view";
pub(crate) const PERM_SURVEYS_MANAGE: &str = "surveys.manage";

/// Hook event names this module listens to.
pub(crate) const TICKET_UPDATED: &str = "ticket.updated";
pub(crate) const STORE_ORDER_COMPLETED: &str = "store.order.completed";
pub(crate) const ORDER_PAID: &str = "order.paid";
pub(crate) const SEND_REQUESTED: &str = "surveys.send_requested";

/// Scheduler job kind used for scheduled distributions.
pub(crate) const SCHEDULER_KIND_SEND: &str = "surveys.send";

/// Question types. `nps` answers are 0–10, `csat` and `rating`
/// answers are 1–5, `text` answers are free text, and
/// `multiple_choice` answers must be one of `options`.
pub(crate) const QUESTION_TYPES: [&str; 5] = ["nps", "csat", "rating", "text", "multiple_choice"];
/// Distribution triggers.
pub(crate) const TRIGGERS: [&str; 4] = ["manual", "ticket_closed", "purchase", "scheduled"];
