//! Knowledgebase - Constants: limits, capability names, event names, statuses.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::{types::*, state::*, helpers::*, module::*, handlers_articles::*, handlers_tickets::*, handlers_categories::*};

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_TITLE_LEN: usize = 200;

pub(crate) const MAX_BODY_LEN: usize = 50_000;

pub(crate) const MAX_TAG_LEN: usize = 50;

pub(crate) const MAX_TAGS: usize = 20;

pub(crate) const MAX_NAME_LEN: usize = 120;

pub(crate) const MAX_TICKET_ID_LEN: usize = 100;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_KNOWLEDGEBASE_VIEW: &str = "knowledgebase:view";

pub(crate) const PERM_KNOWLEDGEBASE_MANAGE: &str = "knowledgebase:manage";
