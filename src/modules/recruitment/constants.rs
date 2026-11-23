//! Recruitment - Constants: limits, capability names, event names, statuses.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
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
use super::{types::*, state::*, helpers::*, module::*, handlers_jobs::*, handlers_applicants::*, handlers_interviews::*, handlers_evaluations::*, handlers_offers::*};

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_TITLE_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 10000;

pub(crate) const MAX_LOCATION_LEN: usize = 200;

pub(crate) const MAX_NAME_LEN: usize = 200;

pub(crate) const MAX_EMAIL_LEN: usize = 254;

pub(crate) const MAX_NOTES_LEN: usize = 5000;

pub(crate) const MAX_REASON_LEN: usize = 1000;

pub(crate) const MAX_SOURCE_LEN: usize = 120;

pub(crate) const MAX_STORAGE_KEY_LEN: usize = 512;

pub(crate) const MAX_CRITERION_LEN: usize = 120;

/// Interview score range (inclusive) and evaluation score range.
pub(crate) const MIN_SCORE: i64 = 1;

pub(crate) const MAX_SCORE: i64 = 5;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_RECRUITMENT_VIEW: &str = "recruitment.view";

pub(crate) const PERM_RECRUITMENT_MANAGE: &str = "recruitment.manage";

/// Hook event names this module listens to.
pub(crate) const INTERVIEW_REMINDER_REQUESTED: &str = "recruitment.interview.reminder_requested";
