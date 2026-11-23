// Site Builder — Event names, permission strings, limits, and other constants.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::sync::OnceLock;
use uuid::Uuid;

use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::rendering::*;
use super::handlers::*;
use super::module::*;

pub const EVENT_THEME_EXISTS: &str = "theme.exists";

pub const EVENT_THEME_RENDER_BLOCK: &str = "theme.render_block";

/// Hook the frontend (and any other module) emits to render a page
/// by slug or id through the builder: payload `{slug}` or
/// `{page_id}`; the site builder answers in `data` with `{found,
/// html, title, tagline, description}`. Only published pages render;
/// drafts answer `found: false`.
pub const EVENT_RENDER_PAGE: &str = "site_builder.render_page";

pub const EVENT_PAGE_PUBLISHED: &str = "site.page_published";

pub const EVENT_PAGE_UNPUBLISHED: &str = "site.page_unpublished";

/// Block types the server-side renderer knows how to render.
///
/// `pricing_table` embeds a pricing table by id: its content carries
/// `pricing_table_id` (the `pricing_tables` module id) and the block
/// renders a placeholder section carrying that id; the live data is
/// fetched from the pricing tables render endpoint
/// (`GET /api/v1/pricing-tables/tables/{id}/render`).
/// `marketplace_storefront` embeds a marketplace storefront: its
/// content carries `vendor_slug` (or `vendor_id`) and the block
/// renders a placeholder section carrying that reference; the live
/// data comes from the marketplace storefront endpoint
/// (`GET /api/v1/pricing-tables/marketplace/storefronts/{slug}`).
/// Both blocks render placeholders only — no pricing or vendor data
/// is resolved here, so the Site Builder never depends on the
/// pricing tables module's store.
pub const BLOCK_TYPES: &[&str] = &[
    "hero",
    "text",
    "image",
    "gallery",
    "form",
    "custom",
    "pricing_table",
    "marketplace_storefront",
];

/// Section layouts.
pub const SECTION_LAYOUTS: &[&str] = &[
    "full",
    "two-column",
    "three-column",
    "sidebar-left",
    "sidebar-right",
];
