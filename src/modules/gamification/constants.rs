// Gamification — shared constants.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;
pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;
pub(crate) const MAX_REASON_LEN: usize = 2000;
pub(crate) const MAX_ACTION_LEN: usize = 64;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_GAMIFICATION_VIEW: &str = "gamification.view";
pub(crate) const PERM_GAMIFICATION_MANAGE: &str = "gamification.manage";

/// Actions the rules engine and badges recognise. A rule or badge
/// naming anything outside this list is rejected, so a typo can
/// never create a rule that never fires.
pub(crate) const KNOWN_ACTIONS: [&str; 8] = [
    "ticket_closed",
    "sale_completed",
    "review_received",
    "community_help",
    "post_created",
    "topic_created",
    "survey_completed",
    "manual_adjustment",
];

/// Hook event names this module listens to (literals repeated here
/// so the module stays decoupled from the tickets / store /
/// surveys / community modules, exactly like the events module
/// repeats the payments constants).
pub(crate) const TICKET_UPDATED: &str = "ticket.updated";
pub(crate) const STORE_ORDER_COMPLETED: &str = "store.order.completed";
pub(crate) const ORDER_PAID: &str = "order.paid";
pub(crate) const SURVEY_RESPONSE_RECEIVED: &str = "surveys.response.received";
pub(crate) const COMMUNITY_POST_REPLIED: &str = "community.post.replied";
pub(crate) const COMMUNITY_TOPIC_CREATED: &str = "community.topic.created";
