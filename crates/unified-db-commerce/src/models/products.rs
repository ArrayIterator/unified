// Diesel models for the products domain of the unified-db-commerce bounded context.
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
// Product Management (2026-11-22-000028_products).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = product_taxonomy)]
pub struct ProductTaxonomy {
    pub id: Uuid,
    pub kind: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub parent_id: Option<Uuid>,
    pub sort_order: i64,
    pub depth: i64,
    pub path: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = product_taxonomy)]
pub struct NewProductTaxonomy {
    pub id: Uuid,
    pub kind: String,
    pub slug: String,
    pub name: String,
    pub parent_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = products)]
pub struct Product {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub kind: String,
    pub status: String,
    pub pricing_model: String,
    pub billing_period: Option<String>,
    pub price: PgNumeric,
    pub price_currency: String,
    pub renewal_price: Option<PgNumeric>,
    pub renewal_price_currency: Option<String>,
    pub trial_days: i32,
    pub vendor_company_id: Option<Uuid>,
    pub group_id: Option<Uuid>,
    pub category_id: Option<Uuid>,
    pub software_id: Option<Uuid>,
    pub license_template_name: Option<String>,
    pub license_seats: i32,
    pub license_download_limit: i32,
    pub license_duration_days: Option<i64>,
    pub license_requires_license: bool,
    pub support_department_id: Option<Uuid>,
    pub support_department_name: Option<String>,
    pub bundle_rule: String,
    pub bundle_discount_bps: Option<i64>,
    pub listed: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = products)]
pub struct NewProduct {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub kind: String,
    pub billing_period: Option<String>,
    pub price: PgNumeric,
    pub renewal_price: Option<PgNumeric>,
    pub renewal_price_currency: Option<String>,
    pub vendor_company_id: Option<Uuid>,
    pub group_id: Option<Uuid>,
    pub category_id: Option<Uuid>,
    pub software_id: Option<Uuid>,
    pub license_template_name: Option<String>,
    pub license_duration_days: Option<i64>,
    pub support_department_id: Option<Uuid>,
    pub support_department_name: Option<String>,
    pub bundle_discount_bps: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = product_options)]
pub struct ProductOption {
    pub id: Uuid,
    pub product_id: Uuid,
    pub name: String,
    pub values: Value,
    pub required: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = product_options)]
pub struct NewProductOption {
    pub id: Uuid,
    pub product_id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = product_variants)]
pub struct ProductVariant {
    pub id: Uuid,
    pub product_id: Uuid,
    pub sku: String,
    pub label: String,
    pub option_values: Value,
    pub price_delta: PgNumeric,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = product_variants)]
pub struct NewProductVariant {
    pub id: Uuid,
    pub product_id: Uuid,
    pub sku: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = product_bundle_items)]
pub struct ProductBundleItem {
    pub id: Uuid,
    pub bundle_product_id: Uuid,
    pub child_product_id: Uuid,
    pub quantity: i32,
    pub price_override: Option<PgNumeric>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = product_bundle_items)]
pub struct NewProductBundleItem {
    pub id: Uuid,
    pub bundle_product_id: Uuid,
    pub child_product_id: Uuid,
    pub price_override: Option<PgNumeric>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = product_price_tiers)]
pub struct ProductPriceTier {
    pub id: Uuid,
    pub product_id: Uuid,
    pub slug: String,
    pub label: String,
    pub price: PgNumeric,
    pub price_currency: String,
    pub renewal_price: Option<PgNumeric>,
    pub renewal_price_currency: Option<String>,
    pub billing_period: Option<String>,
    pub trial_days: i32,
    pub seats: i32,
    pub per_seat: bool,
    pub download_limit: i32,
    pub features: Value,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = product_price_tiers)]
pub struct NewProductPriceTier {
    pub id: Uuid,
    pub product_id: Uuid,
    pub slug: String,
    pub price: PgNumeric,
    pub renewal_price: Option<PgNumeric>,
    pub renewal_price_currency: Option<String>,
    pub billing_period: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = product_regional_prices)]
pub struct ProductRegionalPrice {
    pub id: Uuid,
    pub product_id: Uuid,
    pub region: String,
    pub price: PgNumeric,
    pub price_currency: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = product_regional_prices)]
pub struct NewProductRegionalPrice {
    pub id: Uuid,
    pub product_id: Uuid,
    pub region: String,
    pub price: PgNumeric,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = product_usage_prices)]
pub struct ProductUsagePrice {
    pub id: Uuid,
    pub product_id: Uuid,
    pub metric: String,
    pub unit_price: PgNumeric,
    pub currency: String,
    pub included_units: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = product_usage_prices)]
pub struct NewProductUsagePrice {
    pub id: Uuid,
    pub product_id: Uuid,
    pub metric: String,
    pub unit_price: PgNumeric,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = product_tax_rules)]
pub struct ProductTaxRule {
    pub id: Uuid,
    pub country: String,
    pub name: String,
    pub rate_bps: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = product_tax_rules)]
pub struct NewProductTaxRule {
    pub id: Uuid,
    pub country: String,
    pub name: String,
    pub rate_bps: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = product_inventory)]
pub struct ProductInventory {
    pub id: Uuid,
    pub product_id: Uuid,
    pub variant_id: Option<Uuid>,
    pub quantity: i64,
    pub reserved: i64,
    pub low_stock_threshold: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = product_inventory)]
pub struct NewProductInventory {
    pub id: Uuid,
    pub product_id: Uuid,
    pub variant_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = product_files)]
pub struct ProductFile {
    pub id: Uuid,
    pub product_id: Uuid,
    pub software_id: Option<Uuid>,
    pub software_version: Option<String>,
    pub release_id: Option<Uuid>,
    pub file_id: Option<Uuid>,
    pub filename: Option<String>,
    pub checksum_sha256: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = product_files)]
pub struct NewProductFile {
    pub id: Uuid,
    pub product_id: Uuid,
    pub software_id: Option<Uuid>,
    pub software_version: Option<String>,
    pub release_id: Option<Uuid>,
    pub file_id: Option<Uuid>,
    pub filename: Option<String>,
    pub checksum_sha256: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = product_versions)]
pub struct ProductVersion {
    pub id: Uuid,
    pub product_id: Uuid,
    pub version: String,
    pub software_id: Option<Uuid>,
    pub release_id: Option<Uuid>,
    pub changelog: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = product_versions)]
pub struct NewProductVersion {
    pub id: Uuid,
    pub product_id: Uuid,
    pub version: String,
    pub software_id: Option<Uuid>,
    pub release_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = product_tags)]
pub struct ProductTag {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = product_tags)]
pub struct NewProductTag {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = product_tag_assignments)]
pub struct ProductTagAssignment {
    pub product_id: Uuid,
    pub tag_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = product_tag_assignments)]
pub struct NewProductTagAssignment {
    pub product_id: Uuid,
    pub tag_id: Uuid,
}
