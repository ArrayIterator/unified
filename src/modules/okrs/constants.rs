// OKR / Goals - Event names, permission strings, limits, and other constants.

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

use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_cycles::*;
use super::handlers_objectives::*;
use super::handlers_key_results::*;
use super::hooks::*;
use super::module::*;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_TITLE_LEN: usize = 300;

pub(crate) const MAX_UNIT_LEN: usize = 32;

pub(crate) const MAX_NOTE_LEN: usize = 5000;

pub(crate) const MAX_REASON_LEN: usize = 1000;

pub(crate) const MAX_COMPANY_LEN: usize = 128;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_OKRS_VIEW: &str = "okrs.view";

pub(crate) const PERM_OKRS_MANAGE: &str = "okrs.manage";

/// Objective levels.
pub(crate) const ALLOWED_LEVELS: &[&str] = &["company", "team", "personal"];

/// Check-in confidence values.
pub(crate) const ALLOWED_CONFIDENCE: &[&str] = &["low", "medium", "high"];

/// Link target types.
pub(crate) const ALLOWED_LINK_TYPES: &[&str] = &["project", "task"];
