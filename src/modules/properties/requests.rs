// Properties - Request bodies.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use uuid::Uuid;

use super::*;

// Request bodies

#[derive(Debug, Deserialize)]
pub struct CreatePropertyRequest {
    pub title: String,
    pub description: Option<String>,
    pub slug: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub address: Option<String>,
    pub total_area_sqm: Option<f64>,
    pub company_id: Option<Uuid>,
    pub cover_storage_target: Option<String>,
    pub cover_storage_key: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePropertyRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub slug: Option<String>,
    pub latitude: Option<Option<f64>>,
    pub longitude: Option<Option<f64>>,
    pub address: Option<Option<String>>,
    pub total_area_sqm: Option<Option<f64>>,
    pub company_id: Option<Option<Uuid>>,
    pub cover_storage_target: Option<Option<String>>,
    pub cover_storage_key: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePlanRequest {
    pub name: String,
    pub description: Option<String>,
    pub geometry: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePlanRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub geometry: Option<Option<Value>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateBlockRequest {
    pub title: String,
    pub notes: Option<String>,
    pub geometry: Value,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub price: Option<i64>,
    pub currency: Option<String>,
    pub click_behavior: Option<ClickBehavior>,
    pub click_link_kind: Option<ClickLinkKind>,
    pub click_link_target: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateBlockRequest {
    pub title: Option<String>,
    pub notes: Option<Option<String>>,
    pub geometry: Option<Value>,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub price: Option<i64>,
    pub currency: Option<String>,
    pub click_behavior: Option<ClickBehavior>,
    pub click_link_kind: Option<Option<ClickLinkKind>>,
    pub click_link_target: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct BlockActionRequest {
    /// The buyer / assignee. Defaults to the caller for reserve
    /// and purchase; required for assign. Acting for another user
    /// requires the `properties.manage` capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buyer_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CorrectStatusRequest {
    /// Target status after the correction: `available`,
    /// `reserved`, or `assigned`.
    pub target_status: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buyer_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateLocationSettingsRequest {
    pub provider: Option<String>,
    pub base_url: Option<Option<String>>,
    pub allowed_host: Option<String>,
    pub timeout_secs: Option<u64>,
    pub link_allowlist_hosts: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct LocationSearchQuery {
    pub q: String,
}

#[derive(Debug, Deserialize)]
pub struct LocationReverseQuery {
    pub lat: f64,
    pub lng: f64,
}

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    #[serde(default = "default_export_format")]
    pub format: String,
}

pub(crate) fn default_export_format() -> String {
    "json".to_string()
}
