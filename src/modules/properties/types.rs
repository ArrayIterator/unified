// Properties - Types.

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

// Types

/// Lifecycle of a property: `draft` -> `published` -> `sold_out`,
/// with `archived` reachable from draft / published / sold_out.
/// `archived` is terminal (a property is deleted, not reopened).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PropertyStatus {
    Draft,
    Published,
    SoldOut,
    Archived,
}

impl Default for PropertyStatus {
    fn default() -> Self {
        Self::Draft
    }
}

impl PropertyStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Published => "published",
            Self::SoldOut => "sold_out",
            Self::Archived => "archived",
        }
    }
}

/// One property: a land parcel or a housing development. The
/// developer / owner company is referenced by id only - company
/// rows are never duplicated here (the company module owns them).
/// Media is referenced through the storage module (target + key);
/// no image bytes live in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Property {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub slug: String,
    /// Latitude / longitude of the property marker (-90..90 /
    /// -180..180). The map picker is client-side; the server only
    /// accepts and validates coordinates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// Total area in square metres (declared by the seller; the
    /// per-block areas computed from geometry do not have to sum
    /// to this value - roads and common areas are not blocks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_area_sqm: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<Uuid>,
    pub status: PropertyStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_storage_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_storage_key: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One site plan (master plan) of a property. `geometry` is the
/// plan boundary as GeoJSON (Polygon / MultiPolygon only - a plan
/// boundary is an area, never a line). Blocks carry their own
/// geometry; a plan with no boundary still renders its blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyPlan {
    pub id: Uuid,
    pub property_id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// GeoJSON geometry object (or null when the plan has no
    /// boundary yet). Validated on write; see `validate_geometry`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<Value>,
    /// SHA-256 hex of the canonical geometry JSON, for change
    /// detection. Null when there is no geometry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry_hash: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Lifecycle of a block (plot): `available` -> `reserved` ->
/// `sold` / `assigned`; `reserved` -> `available` (release);
/// `blocked` is admin-only and reachable from any non-sold
/// status; `sold` is terminal except for an audited admin
/// correction (see `correct_block_status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockStatus {
    Available,
    Reserved,
    Sold,
    Assigned,
    Blocked,
}

impl Default for BlockStatus {
    fn default() -> Self {
        Self::Available
    }
}

impl BlockStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Reserved => "reserved",
            Self::Sold => "sold",
            Self::Assigned => "assigned",
            Self::Blocked => "blocked",
        }
    }

    /// Fill colour used for the block in the SVG rendering.
    pub(crate) fn svg_fill(&self) -> &'static str {
        match self {
            Self::Available => "#22c55e",
            Self::Reserved => "#f59e0b",
            Self::Sold => "#ef4444",
            Self::Assigned => "#3b82f6",
            Self::Blocked => "#6b7280",
        }
    }
}

/// What happens when a visitor clicks a block on the public plan.
/// `popup` shows the block title, notes, price, and status;
/// `link` navigates to the configured target instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClickBehavior {
    Popup,
    Link,
}

impl Default for ClickBehavior {
    fn default() -> Self {
        Self::Popup
    }
}

/// The target of a `link` click. `product` and `order` targets
/// are UUIDs (a store product id / order id); a `page` target is
/// a relative path ("/...") or an absolute URL whose host is on
/// the configured link allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClickLinkKind {
    Product,
    Order,
    Page,
}

/// One block (plot) inside a site plan. Geometry is GeoJSON
/// (Polygon / MultiPolygon for a plot, LineString for a drawn
/// line such as a road or boundary marker). `area_sqm` is
/// computed server-side from the geometry for polygons;
/// `length_m` is computed server-side for lines. The buyer is
/// referenced by customer id only - customer rows are never
/// duplicated here (the CRM module owns them).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyBlock {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub property_id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// GeoJSON geometry object. Validated on write.
    pub geometry: Value,
    /// SHA-256 hex of the canonical geometry JSON.
    pub geometry_hash: String,
    /// Computed area in square metres (polygons only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area_sqm: Option<f64>,
    /// Computed length in metres (lines only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length_m: Option<f64>,
    /// Sale price in minor units of `currency` (0 means not
    /// priced yet). JSON carries major units (see `serde_major`).
    #[serde(with = "unified_core::money::serde_major")]
    pub price: i64,
    pub currency: String,
    pub status: BlockStatus,
    /// Customer id of the buyer / assignee (CRM reference only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_customer_id: Option<String>,
    pub click_behavior: ClickBehavior,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click_link_kind: Option<ClickLinkKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click_link_target: Option<String>,
    /// Store order id of the purchase that sold this block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One audited block status change: who moved the block, when,
/// from which status to which, and why. Written on every status
/// transition, including admin corrections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyBlockHistory {
    pub id: Uuid,
    pub block_id: Uuid,
    pub property_id: Uuid,
    pub plan_id: Uuid,
    pub from_status: String,
    pub to_status: String,
    pub actor_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One block purchase: the record the buy flow revolves around.
/// Created by the purchase endpoint (block becomes `reserved`),
/// linked to a store order id by the `properties.order.created`
/// listener, and resolved by the payment listeners.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockPurchase {
    pub id: Uuid,
    pub property_id: Uuid,
    pub plan_id: Uuid,
    pub block_id: Uuid,
    pub buyer_id: String,
    /// Purchase price in minor units of `currency`. JSON carries
    /// major units (see `serde_major`).
    #[serde(with = "unified_core::money::serde_major")]
    pub price: i64,
    pub currency: String,
    /// Store order id, once the store side has created the order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    /// `requested`, `order_created`, `pending_verification`,
    /// `paid`, `rejected`, `cancelled`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One location search result from a geocoder provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceCandidate {
    pub display_name: String,
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub importance: Option<f64>,
}

/// Geocoder provider settings, held in module state and editable
/// through the admin endpoint. `provider` is `none` (manual
/// coordinates only) or `nominatim` (HTTP adapter). `allowed_host`
/// is the only host the adapter may call; `link_allowlist_hosts`
/// are the only hosts an absolute page link target may point to
/// (relative paths need no allowlist entry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocationSettings {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub allowed_host: String,
    pub timeout_secs: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub link_allowlist_hosts: Vec<String>,
}

impl Default for LocationSettings {
    fn default() -> Self {
        Self {
            provider: "none".to_string(),
            base_url: None,
            allowed_host: "nominatim.openstreetmap.org".to_string(),
            timeout_secs: DEFAULT_GEOCODER_TIMEOUT_SECS,
            link_allowlist_hosts: Vec::new(),
        }
    }
}
