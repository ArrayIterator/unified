// hosting module - Requirement-check views.
//
// Split from the former single-file module `src/modules/hosting.rs`.
// The module root (`hosting.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::Domain;
use crate::modules::company::company_exists;
use crate::modules::network::DnsVerifier;
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

// Requirement-check views.

/// One checked requirement of a hosting account.
#[derive(Debug, Clone, Serialize)]
pub struct RequirementItem {
    /// `domain_registered`, `ssl_ready`, `store_product_linked`,
    /// `store_order_paid`, or `plan_exists`.
    pub code: String,
    pub label: String,
    /// `"ok"`, `"failed"`, `"unknown"`, or `"skipped"`.
    pub status: String,
    pub detail: String,
    /// Where the answer came from: `"database"`, `"hook_cache"`,
    /// `"local_state"`, or `"unverified"`.
    pub source: String,
}

/// Full requirement report for one hosting account.
#[derive(Debug, Clone, Serialize)]
pub struct AccountRequirementReport {
    pub account_id: Uuid,
    pub domain: String,
    /// `"ok"` when every applicable check passed, `"not_ready"` when
    /// at least one check failed, `"unknown"` when nothing failed but
    /// at least one check could not be verified.
    pub status: String,
    pub can_activate: bool,
    pub checks: Vec<RequirementItem>,
}
