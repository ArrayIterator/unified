// Elearning - shared constants.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_TITLE_LEN: usize = 200;
pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;
pub(crate) const MAX_TEXT_LEN: usize = 20000;
pub(crate) const MAX_SLUG_LEN: usize = 64;
pub(crate) const MAX_REF_LEN: usize = 500;
pub(crate) const MAX_COMMENT_LEN: usize = 2000;
pub(crate) const MAX_QUESTIONS_PER_QUIZ: usize = 200;
pub(crate) const MAX_OPTIONS_PER_QUESTION: usize = 10;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_ELEARNING_VIEW: &str = "elearning.view";
pub(crate) const PERM_ELEARNING_MANAGE: &str = "elearning.manage";
pub(crate) const PERM_ELEARNING_INSTRUCT: &str = "elearning.instruct";

/// Event names this module listens to.
pub(crate) const ORDER_PAID: &str = "order.paid";
pub(crate) const STORE_ORDER_COMPLETED: &str = "store.order.completed";
pub(crate) const STORE_ORDER_REFUNDED: &str = "store.order.refunded";
pub(crate) const STORE_ORDER_CANCELLED: &str = "store.order.cancelled";
pub(crate) const ANCHOR_CONFIRMED: &str = "web3.anchor.confirmed";
pub(crate) const ANCHOR_FAILED: &str = "web3.anchor.failed";

/// Course lifecycle statuses. Validated against the live settings
/// only through the transition table in `valid_course_transition`;
/// the strings themselves are the stored status values.
pub(crate) const STATUS_DRAFT: &str = "draft";
pub(crate) const STATUS_PUBLISHED: &str = "published";
pub(crate) const STATUS_ARCHIVED: &str = "archived";

/// Enrollment statuses.
pub(crate) const ENROLL_ACTIVE: &str = "active";
pub(crate) const ENROLL_PENDING_PAYMENT: &str = "pending_payment";
pub(crate) const ENROLL_COMPLETED: &str = "completed";
pub(crate) const ENROLL_CANCELLED: &str = "cancelled";
pub(crate) const ENROLL_SUSPENDED: &str = "suspended";
