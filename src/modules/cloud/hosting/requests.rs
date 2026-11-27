// hosting module - Request types.
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

// Request types.

#[derive(Debug, Deserialize)]
pub(crate) struct CreatePlanRequest {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) description: Option<String>,
    pub(crate) disk_gb: i64,
    pub(crate) cpu_cores: f64,
    pub(crate) ram_mb: i64,
    pub(crate) bandwidth_gb: i64,
    #[serde(with = "unified_core::money::serde_major")]
    pub(crate) price: i64,
    #[serde(default)]
    pub(crate) currency: Option<String>,
    pub(crate) billing_cycle: String,
    #[serde(default)]
    pub(crate) store_product_id: Option<String>,
    #[serde(default)]
    pub(crate) is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdatePlanRequest {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) disk_gb: Option<i64>,
    #[serde(default)]
    pub(crate) cpu_cores: Option<f64>,
    #[serde(default)]
    pub(crate) ram_mb: Option<i64>,
    #[serde(default)]
    pub(crate) bandwidth_gb: Option<i64>,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub(crate) price: Option<i64>,
    #[serde(default)]
    pub(crate) currency: Option<String>,
    #[serde(default)]
    pub(crate) billing_cycle: Option<String>,
    #[serde(default)]
    pub(crate) store_product_id: Option<Option<String>>,
    #[serde(default)]
    pub(crate) is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateAccountRequest {
    pub(crate) plan_id: Uuid,
    pub(crate) domain: String,
    #[serde(default)]
    pub(crate) company_id: Option<Uuid>,
    #[serde(default)]
    pub(crate) store_order_id: Option<String>,
    #[serde(default)]
    pub(crate) disk_used_mb: Option<i64>,
    #[serde(default)]
    pub(crate) bandwidth_used_mb: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateAccountRequest {
    #[serde(default)]
    pub(crate) plan_id: Option<Uuid>,
    #[serde(default)]
    pub(crate) company_id: Option<Option<Uuid>>,
    #[serde(default)]
    pub(crate) disk_used_mb: Option<i64>,
    #[serde(default)]
    pub(crate) bandwidth_used_mb: Option<i64>,
}
