// Diesel models for the store domain of the unified-db-core bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Store models.
//
// Money note: Diesel's built-in NUMERIC type is
// `diesel::data_types::PgNumeric`, and it has no serde Serialize /
// Deserialize implementation. The five models below that carry money
// therefore derive Queryable / Selectable / Insertable only, without
// Serialize / Deserialize; the API layer must format money values
// explicitly. If the project later allows a decimal crate (bigdecimal
// or rust_decimal, both with serde support), these fields can switch
// to that type and regain the serde derives. See docs/SCHEMA.md.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = store_products)]
pub struct StoreProduct {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub price: PgNumeric,
    pub currency: String,
    pub kind: String,
    pub license_product: bool,
    pub file_id: Option<Uuid>,
    pub slug: Option<String>,
    pub software_id: Option<Uuid>,
    pub software_slug: Option<String>,
    pub status: String,
    pub company_id: Option<Uuid>,
    pub pricing_model: String,
    pub billing_period: Option<String>,
    pub renewal_price: Option<PgNumeric>,
    pub trial_days: i32,
    pub updates_years: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = store_products)]
pub struct NewStoreProduct {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub price: PgNumeric,
    pub currency: String,
    pub kind: String,
    pub license_product: bool,
    pub file_id: Option<Uuid>,
    pub slug: Option<String>,
    pub software_id: Option<Uuid>,
    pub software_slug: Option<String>,
    pub status: String,
    pub company_id: Option<Uuid>,
    pub pricing_model: String,
    pub billing_period: Option<String>,
    pub renewal_price: Option<PgNumeric>,
    pub trial_days: i32,
    pub updates_years: Option<i32>,
}

// ---------------------------------------------------------------------------
// Pricing tiers / editions per product.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = store_pricing_tiers)]
pub struct StorePricingTier {
    pub id: Uuid,
    pub product_id: Uuid,
    pub slug: String,
    pub label: String,
    pub price: PgNumeric,
    pub renewal_price: Option<PgNumeric>,
    pub billing_period: Option<String>,
    pub trial_days: i32,
    pub seats: i32,
    pub download_limit: i32,
    pub features: Value,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = store_pricing_tiers)]
pub struct NewStorePricingTier {
    pub id: Uuid,
    pub product_id: Uuid,
    pub slug: String,
    pub label: String,
    pub price: PgNumeric,
    pub renewal_price: Option<PgNumeric>,
    pub billing_period: Option<String>,
    pub trial_days: i32,
    pub seats: i32,
    pub download_limit: i32,
    pub features: Value,
    pub status: String,
}

// ---------------------------------------------------------------------------
// Orders with validated lifecycle and coupon totals.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = store_orders)]
pub struct StoreOrder {
    pub id: Uuid,
    pub user_id: String,
    pub subtotal: PgNumeric,
    pub discount_total: PgNumeric,
    pub total: PgNumeric,
    pub currency: String,
    pub coupon_code: Option<String>,
    pub company_id: Option<Uuid>,
    pub trial_until: Option<DateTime<Utc>>,
    pub status: String,
    pub provider: Option<String>,
    pub reference: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = store_orders)]
pub struct NewStoreOrder {
    pub id: Uuid,
    pub user_id: String,
    pub subtotal: PgNumeric,
    pub discount_total: PgNumeric,
    pub total: PgNumeric,
    pub currency: String,
    pub coupon_code: Option<String>,
    pub company_id: Option<Uuid>,
    pub trial_until: Option<DateTime<Utc>>,
    pub status: String,
    pub provider: Option<String>,
    pub reference: String,
}

// ---------------------------------------------------------------------------
// Order line items.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = store_order_items)]
pub struct StoreOrderItem {
    pub id: Uuid,
    pub order_id: Uuid,
    pub product_id: Option<Uuid>,
    pub name: String,
    pub unit_price: PgNumeric,
    pub quantity: i32,
    pub line_total: PgNumeric,
    pub license_product: bool,
    pub file_id: Option<Uuid>,
    pub tier_id: Option<Uuid>,
    pub tier_name: Option<String>,
    pub software_id: Option<Uuid>,
    pub software_slug: Option<String>,
    pub company_id: Option<Uuid>,
    pub pricing_model: String,
    pub billing_period: Option<String>,
    pub seats: i32,
    pub expires_at: Option<DateTime<Utc>>,
    pub download_limit: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = store_order_items)]
pub struct NewStoreOrderItem {
    pub id: Uuid,
    pub order_id: Uuid,
    pub product_id: Option<Uuid>,
    pub name: String,
    pub unit_price: PgNumeric,
    pub quantity: i32,
    pub line_total: PgNumeric,
    pub license_product: bool,
    pub file_id: Option<Uuid>,
    pub tier_id: Option<Uuid>,
    pub tier_name: Option<String>,
    pub software_id: Option<Uuid>,
    pub software_slug: Option<String>,
    pub company_id: Option<Uuid>,
    pub pricing_model: String,
    pub billing_period: Option<String>,
    pub seats: i32,
    pub expires_at: Option<DateTime<Utc>>,
    pub download_limit: i32,
}

// ---------------------------------------------------------------------------
// Download entitlements and download history.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = store_entitlements)]
pub struct StoreEntitlement {
    pub id: Uuid,
    pub license_id: Option<Uuid>,
    pub license_key: Option<String>,
    pub order_id: Option<Uuid>,
    pub user_id: String,
    pub product_id: Option<Uuid>,
    pub tier_id: Option<Uuid>,
    pub software_id: Option<Uuid>,
    pub software_slug: Option<String>,
    pub release_id: Option<Uuid>,
    pub download_limit: i32,
    pub downloads_used: i32,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = store_entitlements)]
pub struct NewStoreEntitlement {
    pub id: Uuid,
    pub license_id: Option<Uuid>,
    pub license_key: Option<String>,
    pub order_id: Option<Uuid>,
    pub user_id: String,
    pub product_id: Option<Uuid>,
    pub tier_id: Option<Uuid>,
    pub software_id: Option<Uuid>,
    pub software_slug: Option<String>,
    pub release_id: Option<Uuid>,
    pub download_limit: i32,
    pub downloads_used: i32,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = store_downloads)]
pub struct StoreDownload {
    pub id: Uuid,
    pub license_id: Option<Uuid>,
    pub release_id: Option<Uuid>,
    pub software_slug: Option<String>,
    pub order_id: Option<Uuid>,
    pub downloaded_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = store_downloads)]
pub struct NewStoreDownload {
    pub id: Uuid,
    pub license_id: Option<Uuid>,
    pub release_id: Option<Uuid>,
    pub software_slug: Option<String>,
    pub order_id: Option<Uuid>,
    pub downloaded_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Update-API catalog: latest known version per software slug.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = store_catalog)]
pub struct StoreCatalog {
    pub slug: String,
    pub name: String,
    pub kind: String,
    pub product_id: Option<Uuid>,
    pub product_name: String,
    pub latest_version: String,
    pub release_id: Option<Uuid>,
    pub file_id: Option<Uuid>,
    pub changelog: String,
    pub requires_wp: Option<String>,
    pub tested_wp: Option<String>,
    pub requires_php: Option<String>,
    pub license_required: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = store_catalog)]
pub struct NewStoreCatalog {
    pub slug: String,
    pub name: String,
    pub kind: String,
    pub product_id: Option<Uuid>,
    pub product_name: String,
    pub latest_version: String,
    pub release_id: Option<Uuid>,
    pub file_id: Option<Uuid>,
    pub changelog: String,
    pub requires_wp: Option<String>,
    pub tested_wp: Option<String>,
    pub requires_php: Option<String>,
    pub license_required: bool,
}

// ---------------------------------------------------------------------------
// Store mirror of licenses issued through `order.created`.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = store_issued_licenses)]
pub struct StoreIssuedLicense {
    pub license_id: Uuid,
    pub license_key: String,
    pub product: String,
    pub order_id: Option<Uuid>,
    pub user_id: Option<String>,
    pub company_id: Option<Uuid>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = store_issued_licenses)]
pub struct NewStoreIssuedLicense {
    pub license_id: Uuid,
    pub license_key: String,
    pub product: String,
    pub order_id: Option<Uuid>,
    pub user_id: Option<String>,
    pub company_id: Option<Uuid>,
    pub status: String,
}
