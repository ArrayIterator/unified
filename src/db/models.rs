// Diesel models for the tables in `schema.rs` (migrations
// 000001-000030).
//
// These are the shared persistence types for every table in the
// schema: users, roles, companies, domains, Geo overrides, licenses,
// all software, store, and VCS tables, and every table added by
// migrations 000004-000030. Modules that keep their own data in an
// in-memory store today use these tables as their persistence target,
// not yet their read path; see TODO.md.
//
// Money fields use `diesel::data_types::PgNumeric` (see the note above
// the store models); models that carry money skip the serde derives
// because `PgNumeric` has no serde implementation.
//
// `New*` structs carry every column except `created_at` /
// `updated_at` and except columns the database fills with a DEFAULT
// (primary-key columns are always carried, callers supply ids);
// the DEFAULT then fires on insert. Secret columns (stored
// credentials, key hashes, credential blobs) carry
// `#[serde(skip_serializing)]` on the read struct, like
// `User.password_hash`.

use crate::db::schema::*;
use chrono::{DateTime, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = users)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    #[serde(skip_serializing)]
    pub totp_secret: Option<String>,
    pub totp_enabled: bool,
    pub is_active: bool,
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = users)]
pub struct NewUser {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub password_hash: String,
    pub company_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = roles)]
pub struct Role {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub is_system: bool,
    pub permissions: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = roles)]
pub struct NewRole {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub is_system: bool,
    pub permissions: Value,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = companies)]
pub struct Company {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = companies)]
pub struct NewCompany {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = company_sites)]
pub struct CompanySite {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub domain: String,
    pub is_primary: bool,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = company_sites)]
pub struct NewCompanySite {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub domain: String,
    pub is_primary: bool,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = domains)]
pub struct Domain {
    pub id: Uuid,
    pub name: String,
    pub company_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    pub is_active: bool,
    pub ssl_status: String,
    pub ssl_expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = domains)]
pub struct NewDomain {
    pub id: Uuid,
    pub name: String,
    pub company_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = geo_overrides)]
pub struct GeoOverride {
    pub id: Uuid,
    pub cidr: String,
    pub country_code: String,
    pub country_name: Option<String>,
    pub city: Option<String>,
    pub asn: Option<i32>,
    pub asn_organization: Option<String>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = geo_overrides)]
pub struct NewGeoOverride {
    pub id: Uuid,
    pub cidr: String,
    pub country_code: String,
    pub country_name: Option<String>,
    pub city: Option<String>,
    pub asn: Option<i32>,
    pub asn_organization: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = licenses)]
pub struct License {
    pub id: Uuid,
    pub license_key: String,
    pub product: String,
    pub user_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub max_activations: i32,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = licenses)]
pub struct NewLicense {
    pub id: Uuid,
    pub license_key: String,
    pub product: String,
    pub user_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub max_activations: i32,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = license_activations)]
pub struct LicenseActivation {
    pub id: Uuid,
    pub license_id: Uuid,
    pub instance_id: String,
    pub hostname: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = license_activations)]
pub struct NewLicenseActivation {
    pub id: Uuid,
    pub license_id: Uuid,
    pub instance_id: String,
    pub hostname: Option<String>,
}

// ---------------------------------------------------------------------------
// User / role assignments (many-to-many).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = user_roles)]
pub struct UserRole {
    pub user_id: Uuid,
    pub role_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = user_roles)]
pub struct NewUserRole {
    pub user_id: Uuid,
    pub role_id: Uuid,
}

// ---------------------------------------------------------------------------
// Software taxonomy: groups and categories (cycle-safe tree).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = software_taxonomy)]
pub struct SoftwareTaxonomy {
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
#[diesel(table_name = software_taxonomy)]
pub struct NewSoftwareTaxonomy {
    pub id: Uuid,
    pub kind: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub parent_id: Option<Uuid>,
    pub sort_order: i64,
    pub depth: i64,
    pub path: String,
}

// ---------------------------------------------------------------------------
// Software records: plugins, themes, software, libraries.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = software)]
pub struct Software {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub kind: String,
    pub description: String,
    pub vendor_company_id: Option<Uuid>,
    pub group_id: Option<Uuid>,
    pub category_id: Option<Uuid>,
    pub status: String,
    pub homepage_url: Option<String>,
    pub repository_url: Option<String>,
    pub documentation_url: Option<String>,
    pub screenshots: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = software)]
pub struct NewSoftware {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub kind: String,
    pub description: String,
    pub vendor_company_id: Option<Uuid>,
    pub group_id: Option<Uuid>,
    pub category_id: Option<Uuid>,
    pub status: String,
    pub homepage_url: Option<String>,
    pub repository_url: Option<String>,
    pub documentation_url: Option<String>,
    pub screenshots: Value,
}

// ---------------------------------------------------------------------------
// Software versions (semver) with platform requirements.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = software_versions)]
pub struct SoftwareVersion {
    pub id: Uuid,
    pub product: String,
    pub version: String,
    pub changelog: String,
    pub file_id: Option<Uuid>,
    pub license_required: bool,
    pub software_id: Option<Uuid>,
    pub requirements: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = software_versions)]
pub struct NewSoftwareVersion {
    pub id: Uuid,
    pub product: String,
    pub version: String,
    pub changelog: String,
    pub file_id: Option<Uuid>,
    pub license_required: bool,
    pub software_id: Option<Uuid>,
    pub requirements: Value,
}

// ---------------------------------------------------------------------------
// Software releases: a version on a channel (stable / beta).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = software_releases)]
pub struct SoftwareRelease {
    pub id: Uuid,
    pub version_id: Uuid,
    pub channel: String,
    pub notes: String,
    pub status: String,
    pub download_count: i64,
    pub created_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = software_releases)]
pub struct NewSoftwareRelease {
    pub id: Uuid,
    pub version_id: Uuid,
    pub channel: String,
    pub notes: String,
    pub status: String,
    pub download_count: i64,
    pub published_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Release files with sha256 / sha512 checksums and integrity status.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = release_files)]
pub struct ReleaseFile {
    pub id: Uuid,
    pub release_id: Uuid,
    pub version_id: Uuid,
    pub filename: String,
    pub size_bytes: i64,
    pub checksum_sha256: Option<String>,
    pub checksum_sha512: Option<String>,
    pub download_url: Option<String>,
    pub storage_path: Option<String>,
    pub integrity_status: String,
    pub verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = release_files)]
pub struct NewReleaseFile {
    pub id: Uuid,
    pub release_id: Uuid,
    pub version_id: Uuid,
    pub filename: String,
    pub size_bytes: i64,
    pub checksum_sha256: Option<String>,
    pub checksum_sha512: Option<String>,
    pub download_url: Option<String>,
    pub storage_path: Option<String>,
    pub integrity_status: String,
    pub verified_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Software tags and their many-to-many assignment to software records.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = software_tags)]
pub struct SoftwareTag {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = software_tags)]
pub struct NewSoftwareTag {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = software_tag_assignments)]
pub struct SoftwareTagAssignment {
    pub software_id: Uuid,
    pub tag_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = software_tag_assignments)]
pub struct NewSoftwareTagAssignment {
    pub software_id: Uuid,
    pub tag_id: Uuid,
}

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
// Discount coupons.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = store_coupons)]
pub struct StoreCoupon {
    pub id: Uuid,
    pub code: String,
    pub discount_type: String,
    pub discount_value: PgNumeric,
    pub expires_at: Option<DateTime<Utc>>,
    pub usage_limit: Option<i32>,
    pub used_count: i32,
    pub product_ids: Value,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = store_coupons)]
pub struct NewStoreCoupon {
    pub id: Uuid,
    pub code: String,
    pub discount_type: String,
    pub discount_value: PgNumeric,
    pub expires_at: Option<DateTime<Utc>>,
    pub usage_limit: Option<i32>,
    pub used_count: i32,
    pub product_ids: Value,
    pub active: bool,
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

// ---------------------------------------------------------------------------
// VCS repository connections (GitHub / GitLab / Bitbucket).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = vcs_connections)]
pub struct VcsConnection {
    pub id: Uuid,
    pub provider: String,
    pub owner: String,
    pub repo: String,
    pub host: Option<String>,
    pub software_id: Option<Uuid>,
    pub enabled: bool,
    pub sync_status: String,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = vcs_connections)]
pub struct NewVcsConnection {
    pub id: Uuid,
    pub provider: String,
    pub owner: String,
    pub repo: String,
    pub host: Option<String>,
    pub software_id: Option<Uuid>,
    pub enabled: bool,
    pub sync_status: String,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

// ---------------------------------------------------------------------------
// VCS version drafts imported from a provider (never auto-published).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = vcs_version_drafts)]
pub struct VcsVersionDraft {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub software_id: Option<Uuid>,
    pub provider: String,
    pub version: String,
    pub tag: String,
    pub title: Option<String>,
    pub changelog: String,
    pub published_at: Option<DateTime<Utc>>,
    pub status: String,
    pub source: String,
    pub assets: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = vcs_version_drafts)]
pub struct NewVcsVersionDraft {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub software_id: Option<Uuid>,
    pub provider: String,
    pub version: String,
    pub tag: String,
    pub title: Option<String>,
    pub changelog: String,
    pub published_at: Option<DateTime<Utc>>,
    pub status: String,
    pub source: String,
    pub assets: Value,
}

// ---------------------------------------------------------------------------
// VCS sync history (manual and webhook-triggered sync runs).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = vcs_sync_history)]
pub struct VcsSyncHistory {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub provider: String,
    pub status: String,
    pub releases_found: i32,
    pub drafts_created: i32,
    pub error: Option<String>,
    pub rate_limit_remaining: Option<i32>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = vcs_sync_history)]
pub struct NewVcsSyncHistory {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub provider: String,
    pub status: String,
    pub releases_found: i32,
    pub drafts_created: i32,
    pub error: Option<String>,
    pub rate_limit_remaining: Option<i32>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: i64,
}

// ---------------------------------------------------------------------------
// VCS provider settings. Tokens live in `*_encrypted` columns and must
// be encrypted before insert; the encryption helper is still open work.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = vcs_provider_settings)]
pub struct VcsProviderSettings {
    pub provider: String,
    pub token_encrypted: Option<String>,
    pub webhook_secret_encrypted: Option<String>,
    pub api_base_override: Option<String>,
    pub gitlab_hosts: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = vcs_provider_settings)]
pub struct NewVcsProviderSettings {
    pub provider: String,
    pub token_encrypted: Option<String>,
    pub webhook_secret_encrypted: Option<String>,
    pub api_base_override: Option<String>,
    pub gitlab_hosts: Value,
}

// ---------------------------------------------------------------------------
// API Management (2026-11-22-000004_api_management).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = api_versions)]
pub struct ApiVersion {
    pub id: String,
    pub number: i32,
    pub enabled: bool,
    pub deprecated: bool,
    pub sunset_date: Option<DateTime<Utc>>,
    pub rate_limit_per_minute: i32,
    pub auth_required: bool,
    pub notes: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = api_versions)]
pub struct NewApiVersion {
    pub id: String,
    pub number: i32,
    pub sunset_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = api_endpoints)]
pub struct ApiEndpoint {
    pub id: Uuid,
    pub method: String,
    pub path: String,
    pub version: String,
    pub description: String,
    pub rate_limit: i32,
    pub enabled: bool,
    pub rules: Value,
    pub owner_module: String,
    pub auth_required: bool,
    pub source: String,
    pub version_support: Value,
    pub execution_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = api_endpoints)]
pub struct NewApiEndpoint {
    pub id: Uuid,
    pub method: String,
    pub path: String,
    pub version: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = api_prefix_settings)]
pub struct ApiPrefixSettings {
    pub key: String,
    pub prefix: String,
    pub migrate_internal: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = api_prefix_settings)]
pub struct NewApiPrefixSettings {
    pub key: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = api_keys)]
pub struct ApiKey {
    pub id: Uuid,
    pub name: String,
    pub owner_id: Option<Uuid>,
    pub prefix: String,
    pub last4: String,
    #[serde(skip_serializing)]
    pub key_hash: String,
    pub scopes: Value,
    pub rate_tier: String,
    pub requests_per_minute: i32,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub grace_until: Option<DateTime<Utc>>,
    pub rotated_from: Option<Uuid>,
    pub rotated_to: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub usage_count: i64,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = api_keys)]
pub struct NewApiKey {
    pub id: Uuid,
    pub name: String,
    pub owner_id: Option<Uuid>,
    pub prefix: String,
    pub last4: String,
    pub key_hash: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub grace_until: Option<DateTime<Utc>>,
    pub rotated_from: Option<Uuid>,
    pub rotated_to: Option<Uuid>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = api_access_rules)]
pub struct ApiAccessRule {
    pub id: Uuid,
    pub method: String,
    pub path: String,
    pub version: Option<String>,
    pub allowed: bool,
    pub public_access: bool,
    pub actor: String,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = api_access_rules)]
pub struct NewApiAccessRule {
    pub id: Uuid,
    pub method: String,
    pub path: String,
    pub version: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = api_access_audit)]
pub struct ApiAccessAudit {
    pub id: Uuid,
    pub actor: String,
    pub action: String,
    pub target: String,
    pub detail: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = api_access_audit)]
pub struct NewApiAccessAudit {
    pub id: Uuid,
    pub actor: String,
    pub action: String,
    pub target: String,
}

// ---------------------------------------------------------------------------
// Certificates (2026-11-22-000005_certificates).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = certificates)]
pub struct Certificate {
    pub id: Uuid,
    pub domain: String,
    pub provider: String,
    pub status: String,
    pub issuer: String,
    pub sans: Value,
    pub fingerprint_sha256: String,
    pub not_before: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub days_remaining: Option<i64>,
    pub self_signed: bool,
    pub wildcard: bool,
    pub source: String,
    pub revoked: bool,
    pub last_renewed_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = certificates)]
pub struct NewCertificate {
    pub id: Uuid,
    pub domain: String,
    pub provider: String,
    pub status: String,
    pub fingerprint_sha256: String,
    pub not_before: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub days_remaining: Option<i64>,
    pub source: String,
    pub last_renewed_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = certificate_renewals)]
pub struct CertificateRenewal {
    pub id: Uuid,
    pub domain: String,
    pub provider: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub new_expires_at: Option<DateTime<Utc>>,
    pub forced: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = certificate_renewals)]
pub struct NewCertificateRenewal {
    pub id: Uuid,
    pub domain: String,
    pub provider: String,
    pub status: String,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub new_expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = certificate_settings)]
pub struct CertificateSettings {
    pub key: String,
    pub enabled: bool,
    pub check_interval_secs: i64,
    pub acme: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = certificate_settings)]
pub struct NewCertificateSettings {
    pub key: String,
}

// ---------------------------------------------------------------------------
// Dashboards and reseller links (2026-11-22-000006_dashboards_reseller).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = reseller_links)]
pub struct ResellerLink {
    pub reseller_id: Uuid,
    pub client_id: Uuid,
    pub verified: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = reseller_links)]
pub struct NewResellerLink {
    pub reseller_id: Uuid,
    pub client_id: Uuid,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = view_as_sessions)]
pub struct ViewAsSession {
    pub admin_id: Uuid,
    pub target_user_id: Uuid,
    pub reason: String,
    pub started_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = view_as_sessions)]
pub struct NewViewAsSession {
    pub admin_id: Uuid,
    pub target_user_id: Uuid,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = dashboard_settings)]
pub struct DashboardSettings {
    pub key: String,
    pub admin_prefix: String,
    pub user_prefix: String,
    pub view_as_ttl_minutes: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = dashboard_settings)]
pub struct NewDashboardSettings {
    pub key: String,
}

// ---------------------------------------------------------------------------
// Hosting (2026-11-22-000007_hosting).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = hosting_plans)]
pub struct HostingPlan {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub disk_gb: i64,
    pub cpu_cores: f64,
    pub ram_mb: i64,
    pub bandwidth_gb: i64,
    pub price: PgNumeric,
    pub currency: String,
    pub billing_cycle: String,
    pub store_product_id: Option<String>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = hosting_plans)]
pub struct NewHostingPlan {
    pub id: Uuid,
    pub name: String,
    pub disk_gb: i64,
    pub cpu_cores: f64,
    pub ram_mb: i64,
    pub bandwidth_gb: i64,
    pub price: PgNumeric,
    pub currency: String,
    pub billing_cycle: String,
    pub store_product_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hosting_accounts)]
pub struct HostingAccount {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub domain: String,
    pub company_id: Option<Uuid>,
    pub status: String,
    pub ssl_state: String,
    pub disk_used_mb: i64,
    pub bandwidth_used_mb: i64,
    pub store_order_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hosting_accounts)]
pub struct NewHostingAccount {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub domain: String,
    pub company_id: Option<Uuid>,
    pub status: String,
    pub store_order_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hosting_order_claims)]
pub struct HostingOrderClaim {
    pub order_id: String,
    pub plan_id: Uuid,
    pub user_id: String,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hosting_order_claims)]
pub struct NewHostingOrderClaim {
    pub order_id: String,
    pub plan_id: Uuid,
    pub user_id: String,
    pub company_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Clusters (2026-11-22-000008_clusters).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = clusters)]
pub struct Cluster {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub kind: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = clusters)]
pub struct NewCluster {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cluster_nodes)]
pub struct ClusterNode {
    pub id: Uuid,
    pub cluster_id: Uuid,
    pub display_name: String,
    pub base_url: String,
    pub role: String,
    pub status: String,
    #[serde(skip_serializing)]
    pub secret_hash: String,
    pub secret_fingerprint: String,
    pub health: String,
    pub consecutive_failures: i32,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub last_latency_ms: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cluster_nodes)]
pub struct NewClusterNode {
    pub id: Uuid,
    pub cluster_id: Uuid,
    pub display_name: String,
    pub base_url: String,
    pub role: String,
    pub status: String,
    pub secret_hash: String,
    pub secret_fingerprint: String,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub last_latency_ms: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cluster_sync_plans)]
pub struct ClusterSyncPlan {
    pub id: Uuid,
    pub cluster_id: Uuid,
    pub target_node_id: Uuid,
    pub local_node_id: String,
    pub direction: String,
    pub domains: Value,
    pub schedule: String,
    pub enabled: bool,
    #[serde(skip_serializing)]
    pub peer_secret_hash: String,
    pub peer_secret_fingerprint: Option<String>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cluster_sync_plans)]
pub struct NewClusterSyncPlan {
    pub id: Uuid,
    pub cluster_id: Uuid,
    pub target_node_id: Uuid,
    pub local_node_id: String,
    pub direction: String,
    pub peer_secret_hash: String,
    pub peer_secret_fingerprint: Option<String>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cluster_sync_items)]
pub struct ClusterSyncItem {
    pub domain: String,
    pub key: String,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cluster_sync_items)]
pub struct NewClusterSyncItem {
    pub domain: String,
    pub key: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cluster_sync_logs)]
pub struct ClusterSyncLog {
    pub id: Uuid,
    pub plan_id: Option<Uuid>,
    pub cluster_id: Uuid,
    pub node_id: Option<Uuid>,
    pub direction: String,
    pub domain: String,
    pub status: String,
    pub items_sent: i64,
    pub items_received: i64,
    pub items_applied: i64,
    pub items_skipped: i64,
    pub conflicts: Value,
    pub errors: Value,
    pub dry_run: bool,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cluster_sync_logs)]
pub struct NewClusterSyncLog {
    pub id: Uuid,
    pub plan_id: Option<Uuid>,
    pub cluster_id: Uuid,
    pub node_id: Option<Uuid>,
    pub direction: String,
    pub domain: String,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cluster_health_checks)]
pub struct ClusterHealthCheck {
    pub id: Uuid,
    pub node_id: Uuid,
    pub cluster_id: Uuid,
    pub status: String,
    pub latency_ms: Option<i64>,
    pub error: Option<String>,
    pub checked_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cluster_health_checks)]
pub struct NewClusterHealthCheck {
    pub id: Uuid,
    pub node_id: Uuid,
    pub cluster_id: Uuid,
    pub status: String,
    pub latency_ms: Option<i64>,
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Backup and storage targets (2026-11-22-000009_backup_storage).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = backup_jobs)]
pub struct BackupJob {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub database_name: String,
    pub paths: Value,
    pub config_keys: Value,
    pub target: String,
    pub schedule: String,
    pub retention_keep_last: i32,
    pub retention_keep_days: i32,
    pub enabled: bool,
    pub scheduling: Value,
    pub status: String,
    pub checksum: Option<String>,
    pub size_bytes: Option<i64>,
    pub storage_key: Option<String>,
    pub last_run_id: Option<Uuid>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = backup_jobs)]
pub struct NewBackupJob {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub target: String,
    pub checksum: Option<String>,
    pub size_bytes: Option<i64>,
    pub storage_key: Option<String>,
    pub last_run_id: Option<Uuid>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = backup_runs)]
pub struct BackupRun {
    pub id: Uuid,
    pub job_id: Option<Uuid>,
    pub job_name: String,
    pub kind: String,
    pub target: String,
    pub storage_key: Option<String>,
    pub status: String,
    pub checksum: Option<String>,
    pub size_bytes: Option<i64>,
    pub file_count: Option<i64>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = backup_runs)]
pub struct NewBackupRun {
    pub id: Uuid,
    pub job_id: Option<Uuid>,
    pub job_name: String,
    pub kind: String,
    pub target: String,
    pub storage_key: Option<String>,
    pub checksum: Option<String>,
    pub size_bytes: Option<i64>,
    pub file_count: Option<i64>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_targets)]
pub struct StorageTarget {
    pub name: String,
    pub kind: String,
    pub config: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_targets)]
pub struct NewStorageTarget {
    pub name: String,
    pub kind: String,
}

// ---------------------------------------------------------------------------
// Providers (2026-11-22-000010_providers).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = provider_connections)]
pub struct ProviderConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub endpoint: Option<String>,
    pub region: Option<String>,
    pub project_id: Option<String>,
    #[serde(skip_serializing)]
    pub credentials: Value,
    pub custom_config: Option<Value>,
    pub enabled: bool,
    pub last_tested_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = provider_connections)]
pub struct NewProviderConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub endpoint: Option<String>,
    pub region: Option<String>,
    pub project_id: Option<String>,
    pub custom_config: Option<Value>,
    pub last_tested_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = provider_servers)]
pub struct ProviderServer {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub remote_id: String,
    pub name: String,
    pub item_kind: String,
    pub status: String,
    pub raw_status: Option<String>,
    pub region: Option<String>,
    pub public_ip: Option<String>,
    pub private_ip: Option<String>,
    pub cpu_cores: Option<i32>,
    pub memory_mb: Option<i64>,
    pub disk_gb: Option<i64>,
    pub image: Option<String>,
    pub remote_created_at: Option<String>,
    pub fetched_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = provider_servers)]
pub struct NewProviderServer {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub remote_id: String,
    pub name: String,
    pub item_kind: String,
    pub status: String,
    pub raw_status: Option<String>,
    pub region: Option<String>,
    pub public_ip: Option<String>,
    pub private_ip: Option<String>,
    pub cpu_cores: Option<i32>,
    pub memory_mb: Option<i64>,
    pub disk_gb: Option<i64>,
    pub image: Option<String>,
    pub remote_created_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Tasks and system records (2026-11-22-000011_tasks_records).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = tasks)]
pub struct Task {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub assignee_id: Option<Uuid>,
    pub reporter_id: Option<Uuid>,
    pub priority: String,
    pub status: String,
    pub due_date: Option<DateTime<Utc>>,
    pub company_id: Option<Uuid>,
    pub project_name: Option<String>,
    pub tags: Value,
    pub estimate_minutes: Option<i32>,
    pub spent_minutes: i32,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = tasks)]
pub struct NewTask {
    pub id: Uuid,
    pub title: String,
    pub assignee_id: Option<Uuid>,
    pub reporter_id: Option<Uuid>,
    pub due_date: Option<DateTime<Utc>>,
    pub company_id: Option<Uuid>,
    pub project_name: Option<String>,
    pub estimate_minutes: Option<i32>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = task_comments)]
pub struct TaskComment {
    pub id: Uuid,
    pub task_id: Uuid,
    pub author_id: Option<Uuid>,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = task_comments)]
pub struct NewTaskComment {
    pub id: Uuid,
    pub task_id: Uuid,
    pub author_id: Option<Uuid>,
    pub body: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = task_checklist_items)]
pub struct TaskChecklistItem {
    pub id: Uuid,
    pub task_id: Uuid,
    pub title: String,
    pub done: bool,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = task_checklist_items)]
pub struct NewTaskChecklistItem {
    pub id: Uuid,
    pub task_id: Uuid,
    pub title: String,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = system_records)]
pub struct SystemRecord {
    pub id: Uuid,
    pub seq: i64,
    pub actor: String,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub summary: String,
    pub details: Value,
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = system_records)]
pub struct NewSystemRecord {
    pub id: Uuid,
    pub seq: i64,
    pub actor: String,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub ip: Option<String>,
}

// ---------------------------------------------------------------------------
// Logs and billing (2026-11-22-000012_logs_billing).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = app_logs)]
pub struct AppLog {
    pub id: Uuid,
    pub seq: i64,
    pub timestamp: DateTime<Utc>,
    pub level: String,
    pub kind: String,
    pub source: String,
    pub message: String,
    pub context: Value,
    pub request_id: Option<String>,
    pub user_id: Option<String>,
    pub method: Option<String>,
    pub path: Option<String>,
    pub status: Option<i32>,
    pub duration_ms: Option<i64>,
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = app_logs)]
pub struct NewAppLog {
    pub id: Uuid,
    pub seq: i64,
    pub level: String,
    pub kind: String,
    pub source: String,
    pub message: String,
    pub request_id: Option<String>,
    pub user_id: Option<String>,
    pub method: Option<String>,
    pub path: Option<String>,
    pub status: Option<i32>,
    pub duration_ms: Option<i64>,
    pub ip: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = logs_settings)]
pub struct LogsSettings {
    pub key: String,
    pub retention_days: i32,
    pub max_entries: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = logs_settings)]
pub struct NewLogsSettings {
    pub key: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = invoices)]
pub struct Invoice {
    pub id: Uuid,
    pub number: String,
    pub order_id: Option<String>,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub subtotal: PgNumeric,
    pub tax_total: PgNumeric,
    pub total: PgNumeric,
    pub currency: String,
    pub status: String,
    pub overdue: bool,
    pub tax_inclusive: bool,
    pub amount_paid: PgNumeric,
    pub credit_total: PgNumeric,
    pub balance: PgNumeric,
    pub issue_date: Option<DateTime<Utc>>,
    pub due_date: Option<DateTime<Utc>>,
    pub payment_terms_days: Option<i32>,
    pub paid_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub reminders_sent: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = invoices)]
pub struct NewInvoice {
    pub id: Uuid,
    pub number: String,
    pub order_id: Option<String>,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub total: PgNumeric,
    pub currency: String,
    pub status: String,
    pub issue_date: Option<DateTime<Utc>>,
    pub due_date: Option<DateTime<Utc>>,
    pub payment_terms_days: Option<i32>,
    pub paid_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = invoice_items)]
pub struct InvoiceItem {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub name: String,
    pub quantity: i32,
    pub unit_price: PgNumeric,
    pub line_total: PgNumeric,
    pub tax_rate: f64,
    pub tax_amount: PgNumeric,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = invoice_items)]
pub struct NewInvoiceItem {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub name: String,
    pub quantity: i32,
    pub unit_price: PgNumeric,
    pub line_total: PgNumeric,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = invoice_payments)]
pub struct InvoicePayment {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub amount: PgNumeric,
    pub method: String,
    pub reference: Option<String>,
    pub recorded_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = invoice_payments)]
pub struct NewInvoicePayment {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub amount: PgNumeric,
    pub method: String,
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = credit_notes)]
pub struct CreditNote {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub number: String,
    pub amount: PgNumeric,
    pub reason: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = credit_notes)]
pub struct NewCreditNote {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub number: String,
    pub amount: PgNumeric,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = billing_settings)]
pub struct BillingSettings {
    pub key: String,
    pub invoice_prefix: String,
    pub default_tax_rate: f64,
    pub tax_inclusive: bool,
    pub payment_terms_days: i32,
    pub reminder_before_days: Value,
    pub reminder_after_days: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = billing_settings)]
pub struct NewBillingSettings {
    pub key: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = invoice_sequences)]
pub struct InvoiceSequence {
    pub name: String,
    pub next_value: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = invoice_sequences)]
pub struct NewInvoiceSequence {
    pub name: String,
}

// ---------------------------------------------------------------------------
// Panels (2026-11-22-000013_panels).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = panel_connections)]
pub struct PanelConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub endpoint: Option<String>,
    #[serde(skip_serializing)]
    pub credentials: Value,
    pub generic_config: Option<Value>,
    pub enabled: bool,
    pub last_tested_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = panel_connections)]
pub struct NewPanelConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub endpoint: Option<String>,
    pub generic_config: Option<Value>,
    pub last_tested_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub last_synced_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = panel_synced_records)]
pub struct PanelSyncedRecord {
    pub connection_id: Uuid,
    pub collection: String,
    pub remote_id: String,
    pub name: String,
    pub status: String,
    pub hash: String,
    pub last_synced_at: DateTime<Utc>,
    pub direction: String,
    pub conflict: bool,
    pub local_changed: bool,
    pub prefer_local: bool,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = panel_synced_records)]
pub struct NewPanelSyncedRecord {
    pub connection_id: Uuid,
    pub collection: String,
    pub remote_id: String,
    pub name: String,
    pub status: String,
    pub hash: String,
    pub direction: String,
}

// ---------------------------------------------------------------------------
// Payments and bank transfer (2026-11-22-000014_payments_bank_transfer).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = payment_connections)]
pub struct PaymentConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub mode: String,
    pub endpoint: Option<String>,
    #[serde(skip_serializing)]
    pub credentials: Value,
    pub custom_config: Option<Value>,
    pub delegate_connection_id: Option<Uuid>,
    pub enabled: bool,
    pub last_tested_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = payment_connections)]
pub struct NewPaymentConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub endpoint: Option<String>,
    pub custom_config: Option<Value>,
    pub delegate_connection_id: Option<Uuid>,
    pub last_tested_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = payment_intents)]
pub struct PaymentIntent {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub provider: String,
    pub customer_id: String,
    pub order_id: Option<String>,
    pub invoice_id: Option<String>,
    pub amount_minor: i64,
    pub currency: String,
    pub status: String,
    pub capture_method: String,
    pub provider_transaction_id: Option<String>,
    pub fee_minor: Option<i64>,
    pub idempotency_key: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = payment_intents)]
pub struct NewPaymentIntent {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub provider: String,
    pub customer_id: String,
    pub order_id: Option<String>,
    pub invoice_id: Option<String>,
    pub amount_minor: i64,
    pub currency: String,
    pub status: String,
    pub capture_method: String,
    pub provider_transaction_id: Option<String>,
    pub fee_minor: Option<i64>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = payment_transactions)]
pub struct PaymentTransaction {
    pub id: Uuid,
    pub intent_id: Option<Uuid>,
    pub connection_id: Uuid,
    pub provider: String,
    pub kind: String,
    pub customer_id: String,
    pub order_id: Option<String>,
    pub invoice_id: Option<String>,
    pub subscription_id: Option<Uuid>,
    pub amount_minor: i64,
    pub currency: String,
    pub fee_minor: Option<i64>,
    pub status: String,
    pub provider_transaction_id: String,
    pub provider_refund_id: Option<String>,
    pub idempotency_key: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = payment_transactions)]
pub struct NewPaymentTransaction {
    pub id: Uuid,
    pub intent_id: Option<Uuid>,
    pub connection_id: Uuid,
    pub provider: String,
    pub kind: String,
    pub customer_id: String,
    pub order_id: Option<String>,
    pub invoice_id: Option<String>,
    pub subscription_id: Option<Uuid>,
    pub amount_minor: i64,
    pub currency: String,
    pub fee_minor: Option<i64>,
    pub status: String,
    pub provider_transaction_id: String,
    pub provider_refund_id: Option<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = payment_methods)]
pub struct PaymentMethod {
    pub id: Uuid,
    pub customer_id: String,
    pub connection_id: Uuid,
    pub provider: String,
    #[serde(skip_serializing)]
    pub provider_token: String,
    pub brand: String,
    pub last4: String,
    pub exp_month: i32,
    pub exp_year: i32,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = payment_methods)]
pub struct NewPaymentMethod {
    pub id: Uuid,
    pub customer_id: String,
    pub connection_id: Uuid,
    pub provider: String,
    pub provider_token: String,
    pub brand: String,
    pub last4: String,
    pub exp_month: i32,
    pub exp_year: i32,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = gateway_subscriptions)]
pub struct GatewaySubscription {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub provider: String,
    pub customer_id: String,
    pub order_id: Option<String>,
    pub plan_id: Option<String>,
    pub amount_minor: i64,
    pub currency: String,
    pub interval: String,
    pub interval_count: i32,
    pub status: String,
    pub provider_subscription_id: String,
    pub payment_method_id: Option<Uuid>,
    pub current_period_end: Option<DateTime<Utc>>,
    pub next_renewal_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = gateway_subscriptions)]
pub struct NewGatewaySubscription {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub provider: String,
    pub customer_id: String,
    pub order_id: Option<String>,
    pub plan_id: Option<String>,
    pub amount_minor: i64,
    pub currency: String,
    pub interval: String,
    pub status: String,
    pub provider_subscription_id: String,
    pub payment_method_id: Option<Uuid>,
    pub current_period_end: Option<DateTime<Utc>>,
    pub next_renewal_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = split_bills)]
pub struct SplitBill {
    pub id: Uuid,
    pub order_id: Option<String>,
    pub invoice_id: Option<String>,
    pub customer_id: String,
    pub total_minor: i64,
    pub currency: String,
    pub status: String,
    pub paid_minor: i64,
    pub remaining_minor: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = split_bills)]
pub struct NewSplitBill {
    pub id: Uuid,
    pub order_id: Option<String>,
    pub invoice_id: Option<String>,
    pub customer_id: String,
    pub total_minor: i64,
    pub currency: String,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = split_parts)]
pub struct SplitPart {
    pub id: Uuid,
    pub split_id: Uuid,
    pub index: i32,
    pub amount_minor: i64,
    pub currency: String,
    pub provider: String,
    pub connection_id: Option<Uuid>,
    pub payer_id: String,
    pub due_date: Option<DateTime<Utc>>,
    pub status: String,
    pub paid_transaction_id: Option<Uuid>,
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = split_parts)]
pub struct NewSplitPart {
    pub id: Uuid,
    pub split_id: Uuid,
    pub index: i32,
    pub amount_minor: i64,
    pub currency: String,
    pub provider: String,
    pub connection_id: Option<Uuid>,
    pub payer_id: String,
    pub due_date: Option<DateTime<Utc>>,
    pub status: String,
    pub paid_transaction_id: Option<Uuid>,
    pub paid_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = bank_accounts)]
pub struct BankAccount {
    pub id: Uuid,
    pub bank_name: String,
    pub account_holder: String,
    pub account_number: String,
    pub swift_bic: Option<String>,
    pub currency: String,
    pub instructions: Option<String>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = bank_accounts)]
pub struct NewBankAccount {
    pub id: Uuid,
    pub bank_name: String,
    pub account_holder: String,
    pub account_number: String,
    pub swift_bic: Option<String>,
    pub currency: String,
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = bank_transfer_verifications)]
pub struct BankTransferVerification {
    pub id: Uuid,
    pub order_id: String,
    pub invoice_id: Option<String>,
    pub customer_id: String,
    pub split_id: Option<Uuid>,
    pub part_id: Option<Uuid>,
    pub bank_account_id: Option<Uuid>,
    pub status: String,
    pub expected_amount_minor: Option<i64>,
    pub currency: String,
    pub proof: Option<Value>,
    pub amount_check: String,
    pub amount_difference_minor: Option<i64>,
    pub transaction_id: Option<String>,
    pub verified_by: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
    pub rejection_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = bank_transfer_verifications)]
pub struct NewBankTransferVerification {
    pub id: Uuid,
    pub order_id: String,
    pub invoice_id: Option<String>,
    pub customer_id: String,
    pub split_id: Option<Uuid>,
    pub part_id: Option<Uuid>,
    pub bank_account_id: Option<Uuid>,
    pub status: String,
    pub expected_amount_minor: Option<i64>,
    pub currency: String,
    pub proof: Option<Value>,
    pub amount_difference_minor: Option<i64>,
    pub transaction_id: Option<String>,
    pub verified_by: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
    pub rejection_reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = bank_transfer_verification_audit)]
pub struct BankTransferVerificationAudit {
    pub id: Uuid,
    pub verification_id: Uuid,
    pub action: String,
    pub actor: String,
    pub actor_role: String,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = bank_transfer_verification_audit)]
pub struct NewBankTransferVerificationAudit {
    pub id: Uuid,
    pub verification_id: Uuid,
    pub action: String,
    pub actor: String,
    pub actor_role: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = bank_transfer_settings)]
pub struct BankTransferSettings {
    pub key: String,
    pub amount_tolerance_minor: i64,
    pub proof_expiry_days: i32,
    pub require_reference_number: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = bank_transfer_settings)]
pub struct NewBankTransferSettings {
    pub key: String,
}

// ---------------------------------------------------------------------------
// Planner and analytics (2026-11-22-000015_planner_analytics).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = capacity_pools)]
pub struct CapacityPool {
    pub id: Uuid,
    pub name: String,
    pub provider_connection_id: Option<String>,
    pub cluster_id: Option<String>,
    pub capacity_cpu_cores: f64,
    pub capacity_ram_gb: f64,
    pub capacity_disk_gb: f64,
    pub capacity_bandwidth_tb: f64,
    pub notes: Option<String>,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub last_server_count: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = capacity_pools)]
pub struct NewCapacityPool {
    pub id: Uuid,
    pub name: String,
    pub provider_connection_id: Option<String>,
    pub cluster_id: Option<String>,
    pub notes: Option<String>,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub last_server_count: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = pool_usage_samples)]
pub struct PoolUsageSample {
    pub id: Uuid,
    pub pool_id: Uuid,
    pub used_cpu_cores: f64,
    pub used_ram_gb: f64,
    pub used_disk_gb: f64,
    pub used_bandwidth_tb: f64,
    pub source: String,
    pub recorded_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = pool_usage_samples)]
pub struct NewPoolUsageSample {
    pub id: Uuid,
    pub pool_id: Uuid,
    pub source: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = allocation_plans)]
pub struct AllocationPlan {
    pub id: Uuid,
    pub pool_id: Uuid,
    pub name: String,
    pub company_id: Option<String>,
    pub project_name: Option<String>,
    pub reserved_cpu_cores: f64,
    pub reserved_ram_gb: f64,
    pub reserved_disk_gb: f64,
    pub reserved_bandwidth_tb: f64,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = allocation_plans)]
pub struct NewAllocationPlan {
    pub id: Uuid,
    pub pool_id: Uuid,
    pub name: String,
    pub company_id: Option<String>,
    pub project_name: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = analytics_sales)]
pub struct AnalyticsSale {
    pub id: Uuid,
    pub order_id: Option<String>,
    pub kind: String,
    pub total: PgNumeric,
    pub currency: String,
    pub products: Value,
    pub company_id: Option<String>,
    pub recorded_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = analytics_sales)]
pub struct NewAnalyticsSale {
    pub id: Uuid,
    pub order_id: Option<String>,
    pub kind: String,
    pub total: PgNumeric,
    pub currency: String,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = analytics_license_snapshots)]
pub struct AnalyticsLicenseSnapshot {
    pub license_id: String,
    #[serde(skip_serializing)]
    pub license_key: Option<String>,
    pub product: String,
    pub status: String,
    pub last_event_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = analytics_license_snapshots)]
pub struct NewAnalyticsLicenseSnapshot {
    pub license_id: String,
    pub license_key: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = analytics_metric_samples)]
pub struct AnalyticsMetricSample {
    pub id: Uuid,
    pub kind: String,
    pub source: Option<String>,
    pub recorded_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = analytics_metric_samples)]
pub struct NewAnalyticsMetricSample {
    pub id: Uuid,
    pub kind: String,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = analytics_health_samples)]
pub struct AnalyticsHealthSample {
    pub id: Uuid,
    pub node_id: String,
    pub cluster_id: Option<String>,
    pub status: String,
    pub latency_ms: Option<i64>,
    pub checked_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = analytics_health_samples)]
pub struct NewAnalyticsHealthSample {
    pub id: Uuid,
    pub node_id: String,
    pub cluster_id: Option<String>,
    pub status: String,
    pub latency_ms: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = analytics_reports)]
pub struct AnalyticsReport {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub metrics: Value,
    pub dimensions: Value,
    pub range_preset: Option<String>,
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub filters: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = analytics_reports)]
pub struct NewAnalyticsReport {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub range_preset: Option<String>,
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = analytics_report_runs)]
pub struct AnalyticsReportRun {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub row_count: i64,
    pub error: Option<String>,
    pub rows: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = analytics_report_runs)]
pub struct NewAnalyticsReportRun {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub status: String,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = analytics_scheduled_reports)]
pub struct AnalyticsScheduledReport {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub schedule: String,
    pub enabled: bool,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = analytics_scheduled_reports)]
pub struct NewAnalyticsScheduledReport {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub schedule: String,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Email and newsletter (2026-11-22-000016_email_newsletter).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = email_connections)]
pub struct EmailConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub domain: Option<String>,
    pub region: Option<String>,
    pub sender_email: Option<String>,
    pub sender_name: Option<String>,
    pub webhook_url: Option<String>,
    #[serde(skip_serializing)]
    pub credentials: Value,
    pub test_mode: bool,
    pub enabled: bool,
    pub last_tested_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = email_connections)]
pub struct NewEmailConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub domain: Option<String>,
    pub region: Option<String>,
    pub sender_email: Option<String>,
    pub sender_name: Option<String>,
    pub webhook_url: Option<String>,
    pub last_tested_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = email_send_log)]
pub struct EmailSendLog {
    pub id: Uuid,
    pub connection_id: Option<Uuid>,
    pub provider: String,
    pub to: String,
    pub subject: String,
    pub status: String,
    pub provider_message_id: Option<String>,
    pub test_mode: bool,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = email_send_log)]
pub struct NewEmailSendLog {
    pub id: Uuid,
    pub connection_id: Option<Uuid>,
    pub provider: String,
    pub to: String,
    pub subject: String,
    pub status: String,
    pub provider_message_id: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = mail_smtp_settings)]
pub struct MailSmtpSettings {
    pub key: String,
    pub host: String,
    pub port: i32,
    pub tls: bool,
    pub username: String,
    #[serde(skip_serializing)]
    pub password_encrypted: String,
    pub sender_name: String,
    pub sender_email: String,
    pub connector: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = mail_smtp_settings)]
pub struct NewMailSmtpSettings {
    pub key: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = mail_log)]
pub struct MailLog {
    pub id: Uuid,
    pub to: String,
    pub subject: String,
    pub status: String,
    pub connector: String,
    pub template_id: Option<Uuid>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = mail_log)]
pub struct NewMailLog {
    pub id: Uuid,
    pub to: String,
    pub subject: String,
    pub status: String,
    pub connector: String,
    pub template_id: Option<Uuid>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = email_templates)]
pub struct EmailTemplate {
    pub id: Uuid,
    pub name: String,
    pub subject: String,
    pub category: String,
    pub html: String,
    pub json: Value,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = email_templates)]
pub struct NewEmailTemplate {
    pub id: Uuid,
    pub name: String,
    pub subject: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = email_template_versions)]
pub struct EmailTemplateVersion {
    pub id: Uuid,
    pub template_id: Uuid,
    pub version: i32,
    pub subject: String,
    pub html: String,
    pub json: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = email_template_versions)]
pub struct NewEmailTemplateVersion {
    pub id: Uuid,
    pub template_id: Uuid,
    pub version: i32,
    pub subject: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = newsletter_lists)]
pub struct NewsletterList {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub default_connection_id: Option<Uuid>,
    pub double_opt_in: bool,
    pub sender_name: Option<String>,
    pub sender_email: Option<String>,
    pub base_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = newsletter_lists)]
pub struct NewNewsletterList {
    pub id: Uuid,
    pub name: String,
    pub default_connection_id: Option<Uuid>,
    pub sender_name: Option<String>,
    pub sender_email: Option<String>,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = newsletter_subscribers)]
pub struct NewsletterSubscriber {
    pub id: Uuid,
    pub list_id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub status: String,
    pub metadata: Value,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub unsubscribed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = newsletter_subscribers)]
pub struct NewNewsletterSubscriber {
    pub id: Uuid,
    pub list_id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub status: String,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub unsubscribed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = newsletter_campaigns)]
pub struct NewsletterCampaign {
    pub id: Uuid,
    pub list_id: Uuid,
    pub name: String,
    pub template_id: Option<Uuid>,
    pub subject: String,
    pub preview_text: Option<String>,
    pub connection_id: Option<Uuid>,
    pub status: String,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub sent_at: Option<DateTime<Utc>>,
    pub stats_sent: i64,
    pub stats_delivered: i64,
    pub stats_opened: i64,
    pub stats_clicked: i64,
    pub stats_bounced: i64,
    pub stats_unsubscribed: i64,
    pub stats_complaints: i64,
    pub stats_unique_opened: i64,
    pub stats_unique_clicked: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = newsletter_campaigns)]
pub struct NewNewsletterCampaign {
    pub id: Uuid,
    pub list_id: Uuid,
    pub name: String,
    pub template_id: Option<Uuid>,
    pub subject: String,
    pub preview_text: Option<String>,
    pub connection_id: Option<Uuid>,
    pub status: String,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub sent_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = newsletter_tracking_events)]
pub struct NewsletterTrackingEvent {
    pub id: Uuid,
    pub campaign_id: Uuid,
    pub subscriber_id: Uuid,
    pub kind: String,
    pub url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = newsletter_tracking_events)]
pub struct NewNewsletterTrackingEvent {
    pub id: Uuid,
    pub campaign_id: Uuid,
    pub subscriber_id: Uuid,
    pub kind: String,
    pub url: Option<String>,
}

// ---------------------------------------------------------------------------
// Load balancer (2026-11-22-000017_load_balancer).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = load_balancers)]
pub struct LoadBalancer {
    pub id: Uuid,
    pub name: String,
    pub listen_address: String,
    pub listen_port: i32,
    pub tls_mode: String,
    pub domain: Option<String>,
    pub certificate_name: Option<String>,
    pub strategy: String,
    pub health_check: Value,
    pub enabled: bool,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = load_balancers)]
pub struct NewLoadBalancer {
    pub id: Uuid,
    pub name: String,
    pub listen_address: String,
    pub listen_port: i32,
    pub domain: Option<String>,
    pub certificate_name: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = lb_backends)]
pub struct LbBackend {
    pub id: Uuid,
    pub balancer_id: Uuid,
    pub name: String,
    pub source_kind: String,
    pub source_id: Option<String>,
    pub address: String,
    pub weight: i32,
    pub enabled: bool,
    pub draining: bool,
    pub health: String,
    pub last_check_at: Option<DateTime<Utc>>,
    pub last_latency_ms: Option<i64>,
    pub consecutive_failures: i32,
    pub consecutive_successes: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = lb_backends)]
pub struct NewLbBackend {
    pub id: Uuid,
    pub balancer_id: Uuid,
    pub name: String,
    pub source_id: Option<String>,
    pub address: String,
    pub last_check_at: Option<DateTime<Utc>>,
    pub last_latency_ms: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = lb_backend_stats)]
pub struct LbBackendStat {
    pub backend_id: Uuid,
    pub requests_routed: i64,
    pub errors: i64,
    pub total_latency_ms: i64,
    pub latency_samples: i64,
    pub checks_total: i64,
    pub checks_ok: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = lb_backend_stats)]
pub struct NewLbBackendStat {
    pub backend_id: Uuid,
}

// ---------------------------------------------------------------------------
// Webhooks, import, and export (2026-11-22-000018_webhooks_import_export).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = webhook_endpoints)]
pub struct WebhookEndpoint {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    pub events: Value,
    #[serde(skip_serializing)]
    pub secret: String,
    pub active: bool,
    pub paused: bool,
    pub max_attempts: i32,
    pub backoff_base_secs: i64,
    pub timeout_secs: i64,
    pub delivery_count: i64,
    pub last_delivery_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = webhook_endpoints)]
pub struct NewWebhookEndpoint {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    pub secret: String,
    pub last_delivery_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = webhook_deliveries)]
pub struct WebhookDelivery {
    pub id: Uuid,
    pub endpoint_id: Uuid,
    pub event: String,
    pub payload: Value,
    pub payload_hash: String,
    pub status: String,
    pub attempts: i32,
    pub max_attempts: i32,
    pub next_retry_at: Option<DateTime<Utc>>,
    pub status_code: Option<i32>,
    pub duration_ms: Option<i64>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = webhook_deliveries)]
pub struct NewWebhookDelivery {
    pub id: Uuid,
    pub endpoint_id: Uuid,
    pub event: String,
    pub payload_hash: String,
    pub status: String,
    pub next_retry_at: Option<DateTime<Utc>>,
    pub status_code: Option<i32>,
    pub duration_ms: Option<i64>,
    pub error: Option<String>,
    pub delivered_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = import_jobs)]
pub struct ImportJob {
    pub id: Uuid,
    pub entity: String,
    pub format: String,
    pub status: String,
    pub total_rows: i64,
    pub valid_rows: i64,
    pub invalid_rows: i64,
    pub row_errors: Value,
    pub results: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = import_jobs)]
pub struct NewImportJob {
    pub id: Uuid,
    pub entity: String,
    pub format: String,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = export_jobs)]
pub struct ExportJob {
    pub id: Uuid,
    pub entity: String,
    pub format: String,
    pub status: String,
    pub fields: Value,
    pub row_count: i64,
    pub data_provider: String,
    pub download_url: String,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = export_jobs)]
pub struct NewExportJob {
    pub id: Uuid,
    pub entity: String,
    pub format: String,
    pub status: String,
    pub data_provider: String,
    pub download_url: String,
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Maintenance, status page, announcements, and auth (2026-11-22-000019_maintenance_statuspage_announcements_auth).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = maintenance_windows)]
pub struct MaintenanceWindow {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub affected_services: Value,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub activated_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = maintenance_windows)]
pub struct NewMaintenanceWindow {
    pub id: Uuid,
    pub title: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub status: String,
    pub activated_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = maintenance_history)]
pub struct MaintenanceHistory {
    pub id: Uuid,
    pub window_id: Uuid,
    pub event: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = maintenance_history)]
pub struct NewMaintenanceHistory {
    pub id: Uuid,
    pub window_id: Uuid,
    pub event: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = status_components)]
pub struct StatusComponent {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub group: Option<String>,
    pub status: String,
    pub sort_order: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = status_components)]
pub struct NewStatusComponent {
    pub id: Uuid,
    pub name: String,
    pub group: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = status_daily_uptime)]
pub struct StatusDailyUptime {
    pub component_id: Uuid,
    pub date: String,
    pub uptime_percent: f64,
    pub minutes_down: i64,
    pub total_minutes: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = status_daily_uptime)]
pub struct NewStatusDailyUptime {
    pub component_id: Uuid,
    pub date: String,
    pub uptime_percent: f64,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = status_incidents)]
pub struct StatusIncident {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub status: String,
    pub severity: String,
    pub affected_components: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = status_incidents)]
pub struct NewStatusIncident {
    pub id: Uuid,
    pub title: String,
    pub status: String,
    pub severity: String,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = status_incident_updates)]
pub struct StatusIncidentUpdate {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub status: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = status_incident_updates)]
pub struct NewStatusIncidentUpdate {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub status: String,
    pub message: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = announcements)]
pub struct Announcement {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub audience: String,
    pub status: String,
    pub publish_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub pinned: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = announcements)]
pub struct NewAnnouncement {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub status: String,
    pub publish_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub published_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = changelog_entries)]
pub struct ChangelogEntry {
    pub id: Uuid,
    pub version: String,
    pub date: String,
    pub category: String,
    pub title: String,
    pub body: String,
    pub link: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = changelog_entries)]
pub struct NewChangelogEntry {
    pub id: Uuid,
    pub version: String,
    pub date: String,
    pub title: String,
    pub body: String,
    pub link: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = announcement_read_receipts)]
pub struct AnnouncementReadReceipt {
    pub announcement_id: Uuid,
    pub user_id: String,
    pub read_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = announcement_read_receipts)]
pub struct NewAnnouncementReadReceipt {
    pub announcement_id: Uuid,
    pub user_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = auth_recovery_codes)]
pub struct AuthRecoveryCode {
    pub user_id: String,
    pub code_hash: String,
    pub used: bool,
    pub created_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = auth_recovery_codes)]
pub struct NewAuthRecoveryCode {
    pub user_id: String,
    pub code_hash: String,
    pub used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = google_accounts)]
pub struct GoogleAccount {
    pub account_id: String,
    pub email: String,
    pub name: Option<String>,
    #[serde(skip_serializing)]
    pub access_token_encrypted: String,
    #[serde(skip_serializing)]
    pub refresh_token_encrypted: String,
    pub expires_at: DateTime<Utc>,
    pub scopes: Value,
    pub linked_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = google_accounts)]
pub struct NewGoogleAccount {
    pub account_id: String,
    pub email: String,
    pub name: Option<String>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = google_drive_links)]
pub struct GoogleDriveLink {
    pub id: Uuid,
    pub name: String,
    pub account_id: String,
    pub email: String,
    pub folder_id: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = google_drive_links)]
pub struct NewGoogleDriveLink {
    pub id: Uuid,
    pub name: String,
    pub account_id: String,
    pub email: String,
    pub folder_id: Option<String>,
    pub status: String,
}

// ---------------------------------------------------------------------------
// Security (2026-11-22-000020_security).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = security_ip_lists)]
pub struct SecurityIpList {
    pub list: String,
    pub cidr: String,
    pub network: String,
    pub prefix: i32,
    pub added_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = security_ip_lists)]
pub struct NewSecurityIpList {
    pub list: String,
    pub cidr: String,
    pub network: String,
    pub prefix: i32,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = security_reputation)]
pub struct SecurityReputation {
    pub ip: String,
    pub score: i32,
    pub flagged: bool,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = security_reputation)]
pub struct NewSecurityReputation {
    pub ip: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = security_failed_auth_clusters)]
pub struct SecurityFailedAuthCluster {
    pub key: String,
    pub ip: Option<String>,
    pub account: Option<String>,
    pub count: i64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub locked: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = security_failed_auth_clusters)]
pub struct NewSecurityFailedAuthCluster {
    pub key: String,
    pub ip: Option<String>,
    pub account: Option<String>,
}

// ---------------------------------------------------------------------------
// Named routes (2026-11-22-000021_routes).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = named_routes)]
pub struct NamedRoute {
    pub name: String,
    pub default_path: String,
    pub current_path: String,
    pub source: String,
    pub description: String,
    pub config_override: Option<String>,
    pub settings_override: Option<String>,
    pub previous_path: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = named_routes)]
pub struct NewNamedRoute {
    pub name: String,
    pub default_path: String,
    pub current_path: String,
    pub config_override: Option<String>,
    pub settings_override: Option<String>,
    pub previous_path: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = route_redirects)]
pub struct RouteRedirect {
    pub route: String,
    pub old_path: String,
    pub new_path: String,
    pub changed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = route_redirects)]
pub struct NewRouteRedirect {
    pub route: String,
    pub old_path: String,
    pub new_path: String,
}

// ---------------------------------------------------------------------------
// Cloudflare (2026-11-22-000022_cloudflare).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cloudflare_settings)]
pub struct CloudflareSettings {
    pub key: String,
    #[serde(skip_serializing)]
    pub api_token_encrypted: String,
    pub account_id: Option<String>,
    pub default_zone_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cloudflare_settings)]
pub struct NewCloudflareSettings {
    pub key: String,
    pub account_id: Option<String>,
    pub default_zone_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cloudflare_purge_history)]
pub struct CloudflarePurgeHistory {
    pub id: Uuid,
    pub zone_id: String,
    pub kind: String,
    pub targets: Value,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cloudflare_purge_history)]
pub struct NewCloudflarePurgeHistory {
    pub id: Uuid,
    pub zone_id: String,
    pub kind: String,
    pub status: String,
}

// ---------------------------------------------------------------------------
// Search, notifications, and saved views (2026-11-22-000023_search_notifications_saved_views).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = search_documents)]
pub struct SearchDocument {
    pub kind: String,
    pub id: String,
    pub title: String,
    pub snippet: String,
    pub url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = search_documents)]
pub struct NewSearchDocument {
    pub kind: String,
    pub id: String,
    pub title: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = notifications)]
pub struct Notification {
    pub id: Uuid,
    pub user_id: String,
    pub title: String,
    pub body: String,
    pub kind: String,
    pub read: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = notifications)]
pub struct NewNotification {
    pub id: Uuid,
    pub user_id: String,
    pub title: String,
    pub body: String,
    pub kind: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = notification_preferences)]
pub struct NotificationPreference {
    pub user_id: String,
    pub channels: Value,
    pub categories: Value,
    pub channel_by_category: Value,
    pub digest: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = notification_preferences)]
pub struct NewNotificationPreference {
    pub user_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = notification_templates)]
pub struct NotificationTemplate {
    pub id: Uuid,
    pub event: String,
    pub locale: String,
    pub title_template: String,
    pub body_template: String,
    pub channels: Value,
    pub email_connection_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = notification_templates)]
pub struct NewNotificationTemplate {
    pub id: Uuid,
    pub event: String,
    pub title_template: String,
    pub body_template: String,
    pub email_connection_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = saved_views)]
pub struct SavedView {
    pub id: Uuid,
    pub user_id: String,
    pub module_id: String,
    pub name: String,
    pub filter: Value,
    pub sort: Option<String>,
    pub order: Option<String>,
    pub visibility: String,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = saved_views)]
pub struct NewSavedView {
    pub id: Uuid,
    pub user_id: String,
    pub module_id: String,
    pub name: String,
    pub sort: Option<String>,
    pub order: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timeline_entries)]
pub struct TimelineEntry {
    pub id: Uuid,
    pub seq: i64,
    pub actor: String,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub summary: String,
    pub source: String,
    pub details: Value,
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timeline_entries)]
pub struct NewTimelineEntry {
    pub id: Uuid,
    pub seq: i64,
    pub actor: String,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub source: String,
    pub ip: Option<String>,
}

// ---------------------------------------------------------------------------
// Audit (2026-11-22-000024_audit).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = audit_entries)]
pub struct AuditEntry {
    pub id: Uuid,
    pub seq: i64,
    pub actor: String,
    pub action: String,
    pub resource: String,
    pub metadata: Value,
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = audit_entries)]
pub struct NewAuditEntry {
    pub id: Uuid,
    pub seq: i64,
    pub actor: String,
    pub action: String,
    pub resource: String,
    pub ip: Option<String>,
}

// ---------------------------------------------------------------------------
// Gateway (2026-11-22-000025_gateway).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = gateway_routing_rules)]
pub struct GatewayRoutingRule {
    pub id: Uuid,
    pub name: String,
    pub priority: i32,
    pub enabled: bool,
    pub host: Option<String>,
    pub path_prefix: Option<String>,
    pub path_regex: Option<String>,
    pub headers: Value,
    pub upstream: Value,
    pub strategy: String,
    pub require_tls: bool,
    pub rate_limit_override: Option<String>,
    pub health_check: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = gateway_routing_rules)]
pub struct NewGatewayRoutingRule {
    pub id: Uuid,
    pub name: String,
    pub host: Option<String>,
    pub path_prefix: Option<String>,
    pub path_regex: Option<String>,
    pub strategy: String,
    pub rate_limit_override: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = gateway_request_log)]
pub struct GatewayRequestLog {
    pub id: Uuid,
    pub request_id: String,
    pub method: String,
    pub host: String,
    pub path: String,
    pub status: i32,
    pub duration_ms: i64,
    pub upstream: Option<String>,
    pub backend_id: Option<Uuid>,
    pub recorded_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = gateway_request_log)]
pub struct NewGatewayRequestLog {
    pub id: Uuid,
    pub request_id: String,
    pub method: String,
    pub host: String,
    pub path: String,
    pub status: i32,
    pub duration_ms: i64,
    pub upstream: Option<String>,
    pub backend_id: Option<Uuid>,
}

// ---------------------------------------------------------------------------
// Departments (CORE) (2026-11-22-000026_departments).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = departments)]
pub struct Department {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub email: Option<String>,
    pub status: String,
    pub is_default: bool,
    pub sla_first_response_minutes: i32,
    pub sla_resolution_minutes: i32,
    pub working_hours: Value,
    pub auto_reply: Value,
    pub agents: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = departments)]
pub struct NewDepartment {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = department_routing_rules)]
pub struct DepartmentRoutingRule {
    pub id: Uuid,
    pub department_id: Uuid,
    pub match_type: String,
    pub match_value: String,
    pub sort_order: i32,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = department_routing_rules)]
pub struct NewDepartmentRoutingRule {
    pub id: Uuid,
    pub department_id: Uuid,
    pub match_type: String,
    pub match_value: String,
}

// ---------------------------------------------------------------------------
// Meetings (2026-11-22-000027_meetings).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = meeting_rooms)]
pub struct MeetingRoom {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub host_id: String,
    pub status: String,
    pub access_model: String,
    pub product_id: Option<String>,
    pub max_participants: i32,
    pub waiting_room_enabled: bool,
    pub join_token: String,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub timezone: String,
    pub recurrence_rule: Option<String>,
    pub scheduler_job_id: Option<String>,
    pub scheduler_status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = meeting_rooms)]
pub struct NewMeetingRoom {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub host_id: String,
    pub status: String,
    pub product_id: Option<String>,
    pub join_token: String,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub recurrence_rule: Option<String>,
    pub scheduler_job_id: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = meeting_participants)]
pub struct MeetingParticipant {
    pub room_id: Uuid,
    pub user_id: String,
    pub role: String,
    pub status: String,
    pub joined_at: Option<DateTime<Utc>>,
    pub left_at: Option<DateTime<Utc>>,
    pub last_seen: DateTime<Utc>,
    pub audio_muted: bool,
    pub video_muted: bool,
    pub sharing_screen: bool,
    pub hand_raised: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = meeting_participants)]
pub struct NewMeetingParticipant {
    pub room_id: Uuid,
    pub user_id: String,
    pub joined_at: Option<DateTime<Utc>>,
    pub left_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = meeting_messages)]
pub struct MeetingMessage {
    pub id: Uuid,
    pub room_id: Uuid,
    pub author_id: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = meeting_messages)]
pub struct NewMeetingMessage {
    pub id: Uuid,
    pub room_id: Uuid,
    pub author_id: String,
    pub body: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = meeting_recordings)]
pub struct MeetingRecording {
    pub id: Uuid,
    pub room_id: Uuid,
    pub started_by: String,
    pub status: String,
    pub storage_target: Option<String>,
    pub storage_key: Option<String>,
    pub storage_url: Option<String>,
    pub size_bytes: Option<i64>,
    pub duration_secs: Option<i64>,
    pub started_at: DateTime<Utc>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = meeting_recordings)]
pub struct NewMeetingRecording {
    pub id: Uuid,
    pub room_id: Uuid,
    pub started_by: String,
    pub storage_target: Option<String>,
    pub storage_key: Option<String>,
    pub storage_url: Option<String>,
    pub size_bytes: Option<i64>,
    pub duration_secs: Option<i64>,
    pub stopped_at: Option<DateTime<Utc>>,
}

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

// ---------------------------------------------------------------------------
// MCP servers (2026-11-22-000029_mcp).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = mcp_servers)]
pub struct McpServer {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub owner_id: Uuid,
    pub company_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    pub product_id: Option<Uuid>,
    pub status: String,
    pub is_public: bool,
    pub allowed_tools: Value,
    pub rate_limit_per_minute: i32,
    pub read_only_role: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = mcp_servers)]
pub struct NewMcpServer {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub owner_id: Uuid,
    pub company_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    pub product_id: Option<Uuid>,
    pub read_only_role: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = mcp_settings)]
pub struct McpSettings {
    pub key: String,
    pub read_only_role: Option<String>,
    pub default_rate_limit_per_minute: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = mcp_settings)]
pub struct NewMcpSettings {
    pub key: String,
    pub read_only_role: Option<String>,
}

// ---------------------------------------------------------------------------
// AI (2026-11-22-000030_ai).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = ai_provider_connections)]
pub struct AiProviderConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub model: String,
    #[serde(skip_serializing)]
    pub api_key_encrypted: Option<String>,
    pub key_fingerprint: Option<String>,
    pub enabled: bool,
    pub rate_limit_per_minute: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = ai_provider_connections)]
pub struct NewAiProviderConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub model: String,
    pub api_key_encrypted: Option<String>,
    pub key_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = ai_usage_records)]
pub struct AiUsageRecord {
    pub id: Uuid,
    pub provider_id: Uuid,
    pub provider_kind: String,
    pub actor: String,
    pub use_case: String,
    pub model: String,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
    pub estimated_cost_usd: Option<f64>,
    pub latency_ms: i64,
    pub status: String,
    pub error_code: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = ai_usage_records)]
pub struct NewAiUsageRecord {
    pub id: Uuid,
    pub provider_id: Uuid,
    pub provider_kind: String,
    pub actor: String,
    pub use_case: String,
    pub model: String,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
    pub estimated_cost_usd: Option<f64>,
    pub status: String,
    pub error_code: Option<i32>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = ai_drafts)]
pub struct AiDraft {
    pub id: Uuid,
    pub kind: String,
    pub title: Option<String>,
    pub subject: Option<String>,
    pub body: String,
    pub prompt: Option<String>,
    pub provider_id: Option<Uuid>,
    pub provider_kind: String,
    pub model: String,
    pub actor: String,
    pub status: String,
    pub ai_generated: bool,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub sources: Value,
    pub usage_id: Option<Uuid>,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = ai_drafts)]
pub struct NewAiDraft {
    pub id: Uuid,
    pub kind: String,
    pub title: Option<String>,
    pub subject: Option<String>,
    pub body: String,
    pub prompt: Option<String>,
    pub provider_id: Option<Uuid>,
    pub provider_kind: String,
    pub model: String,
    pub actor: String,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub usage_id: Option<Uuid>,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
}
