// Diesel models for the tables in `schema.rs` (migrations
// 000001-000107).
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
use chrono::{DateTime, NaiveDate, Utc};
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
    pub company_id: Option<Uuid>,
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
    pub company_id: Option<Uuid>,
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
    pub company_id: Option<Uuid>,
    pub verified: Option<bool>,
    pub last_verified_at: Option<DateTime<Utc>>,
    pub manifest_verified: Option<bool>,
    pub verification_error: Option<String>,
    pub last_drill_at: Option<DateTime<Utc>>,
    pub drill_verified: Option<bool>,
    pub encryption: Option<String>,
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
    pub company_id: Option<Uuid>,
    pub verified: Option<bool>,
    pub last_verified_at: Option<DateTime<Utc>>,
    pub manifest_verified: Option<bool>,
    pub verification_error: Option<String>,
    pub last_drill_at: Option<DateTime<Utc>>,
    pub drill_verified: Option<bool>,
    pub encryption: Option<String>,
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
    pub reference: String,
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
    pub reference: String,
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
    pub actor: String,
    pub company_id: Option<Uuid>,
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
    pub company_id: Option<Uuid>,
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
    pub actor: String,
    pub company_id: Option<Uuid>,
    pub pii_redacted: bool,
    pub redacted_fields: Value,
    pub row_limit: i64,
    pub row_offset: i64,
    pub has_more: bool,
    pub rows_excluded_by_tenant_scope: i64,
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
    pub company_id: Option<Uuid>,
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
    pub user_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub denial_reason: Option<String>,
    pub used_personal_key: bool,
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
    pub user_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub denial_reason: Option<String>,
    pub used_personal_key: bool,
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

// ---------------------------------------------------------------------------
// Project Management (2026-11-22-000031_projects).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = projects)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub methodology: String,
    pub status: String,
    pub visibility: String,
    pub owner_id: String,
    pub company_id: Option<Uuid>,
    pub client_id: Option<String>,
    pub start_date: Option<DateTime<Utc>>,
    pub due_date: Option<DateTime<Utc>>,
    pub color: Option<String>,
    pub icon: Option<String>,
    pub labels: Value,
    pub custom_field_definitions: Value,
    pub archived: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = projects)]
pub struct NewProject {
    pub id: Uuid,
    pub name: String,
    pub methodology: String,
    pub owner_id: String,
    pub company_id: Option<Uuid>,
    pub client_id: Option<String>,
    pub start_date: Option<DateTime<Utc>>,
    pub due_date: Option<DateTime<Utc>>,
    pub color: Option<String>,
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_members)]
pub struct ProjectMember {
    pub id: Uuid,
    pub project_id: Uuid,
    pub user_id: String,
    pub role: String,
    pub joined_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_members)]
pub struct NewProjectMember {
    pub id: Uuid,
    pub project_id: Uuid,
    pub user_id: String,
    pub role: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_boards)]
pub struct ProjectBoard {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub kind: String,
    pub wip_limit: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_boards)]
pub struct NewProjectBoard {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub kind: String,
    pub wip_limit: Option<i32>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_board_columns)]
pub struct ProjectBoardColumn {
    pub id: Uuid,
    pub board_id: Uuid,
    pub name: String,
    pub position: i32,
    pub status_key: String,
    pub wip_limit: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_board_columns)]
pub struct NewProjectBoardColumn {
    pub id: Uuid,
    pub board_id: Uuid,
    pub name: String,
    pub status_key: String,
    pub wip_limit: Option<i32>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_milestones)]
pub struct ProjectMilestone {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub description: String,
    pub due_date: Option<DateTime<Utc>>,
    pub reached: bool,
    pub reached_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_milestones)]
pub struct NewProjectMilestone {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub due_date: Option<DateTime<Utc>>,
    pub reached_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_sprints)]
pub struct ProjectSprint {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub goal: String,
    pub status: String,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
    pub velocity_planned: Option<f64>,
    pub velocity_actual: Option<f64>,
    pub standup_notes: Value,
    pub review_notes: Option<String>,
    pub retrospective_notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_sprints)]
pub struct NewProjectSprint {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
    pub velocity_planned: Option<f64>,
    pub velocity_actual: Option<f64>,
    pub review_notes: Option<String>,
    pub retrospective_notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_phases)]
pub struct ProjectPhase {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub kind: String,
    pub position: i32,
    pub status: String,
    pub gate_status: String,
    pub gate_decision: Option<String>,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_phases)]
pub struct NewProjectPhase {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub gate_decision: Option<String>,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_cycles)]
pub struct ProjectCycle {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub appetite_weeks: i32,
    pub status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub cooldown_weeks: i32,
    pub pitch: Option<String>,
    pub betting_decision: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_cycles)]
pub struct NewProjectCycle {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub started_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub pitch: Option<String>,
    pub betting_decision: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_program_increments)]
pub struct ProjectProgramIncrement {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub status: String,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
    pub objectives: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_program_increments)]
pub struct NewProjectProgramIncrement {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_business_cases)]
pub struct ProjectBusinessCase {
    pub id: Uuid,
    pub project_id: Uuid,
    pub title: String,
    pub justification: String,
    pub benefits: Value,
    pub costs: Value,
    pub risks: Value,
    pub status: String,
    pub exception_report: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_business_cases)]
pub struct NewProjectBusinessCase {
    pub id: Uuid,
    pub project_id: Uuid,
    pub title: String,
    pub exception_report: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_tasks)]
pub struct ProjectTask {
    pub id: Uuid,
    pub project_id: Uuid,
    pub board_id: Option<Uuid>,
    pub parent_id: Option<Uuid>,
    pub epic_id: Option<Uuid>,
    pub title: String,
    pub description: String,
    pub task_type: String,
    pub status: String,
    pub priority: String,
    pub assignee_id: Option<String>,
    pub reporter_id: String,
    pub estimate_points: Option<f64>,
    pub estimate_hours: Option<f64>,
    pub labels: Value,
    pub custom_values: Value,
    pub start_date: Option<DateTime<Utc>>,
    pub due_date: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub sprint_id: Option<Uuid>,
    pub phase_id: Option<Uuid>,
    pub cycle_id: Option<Uuid>,
    pub pi_id: Option<Uuid>,
    pub milestone_id: Option<Uuid>,
    pub process_group: Option<String>,
    pub knowledge_area: Option<String>,
    pub waste_flag: Option<bool>,
    pub pair_partner_id: Option<String>,
    pub test_first: bool,
    pub dmaic_phase: Option<String>,
    pub appetite_weeks: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_tasks)]
pub struct NewProjectTask {
    pub id: Uuid,
    pub project_id: Uuid,
    pub board_id: Option<Uuid>,
    pub parent_id: Option<Uuid>,
    pub epic_id: Option<Uuid>,
    pub title: String,
    pub status: String,
    pub assignee_id: Option<String>,
    pub reporter_id: String,
    pub estimate_points: Option<f64>,
    pub estimate_hours: Option<f64>,
    pub start_date: Option<DateTime<Utc>>,
    pub due_date: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub sprint_id: Option<Uuid>,
    pub phase_id: Option<Uuid>,
    pub cycle_id: Option<Uuid>,
    pub pi_id: Option<Uuid>,
    pub milestone_id: Option<Uuid>,
    pub process_group: Option<String>,
    pub knowledge_area: Option<String>,
    pub waste_flag: Option<bool>,
    pub pair_partner_id: Option<String>,
    pub dmaic_phase: Option<String>,
    pub appetite_weeks: Option<i32>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_task_dependencies)]
pub struct ProjectTaskDependency {
    pub id: Uuid,
    pub task_id: Uuid,
    pub depends_on_task_id: Uuid,
    pub kind: String,
    pub lag_days: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_task_dependencies)]
pub struct NewProjectTaskDependency {
    pub id: Uuid,
    pub task_id: Uuid,
    pub depends_on_task_id: Uuid,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_task_comments)]
pub struct ProjectTaskComment {
    pub id: Uuid,
    pub task_id: Uuid,
    pub author_id: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_task_comments)]
pub struct NewProjectTaskComment {
    pub id: Uuid,
    pub task_id: Uuid,
    pub author_id: String,
    pub body: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_task_watchers)]
pub struct ProjectTaskWatcher {
    pub id: Uuid,
    pub task_id: Uuid,
    pub user_id: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_task_watchers)]
pub struct NewProjectTaskWatcher {
    pub id: Uuid,
    pub task_id: Uuid,
    pub user_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_task_attachments)]
pub struct ProjectTaskAttachment {
    pub id: Uuid,
    pub task_id: Uuid,
    pub target: String,
    pub key: String,
    pub stored_key: String,
    pub filename: String,
    pub content_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub uploaded_by: String,
    pub uploaded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_task_attachments)]
pub struct NewProjectTaskAttachment {
    pub id: Uuid,
    pub task_id: Uuid,
    pub target: String,
    pub key: String,
    pub stored_key: String,
    pub filename: String,
    pub content_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub uploaded_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_time_entries)]
pub struct ProjectTimeEntry {
    pub id: Uuid,
    pub task_id: Uuid,
    pub project_id: Uuid,
    pub user_id: String,
    pub description: String,
    pub hours: f64,
    pub billable: bool,
    pub billed: bool,
    pub invoice_id: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub timer_running: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_time_entries)]
pub struct NewProjectTimeEntry {
    pub id: Uuid,
    pub task_id: Uuid,
    pub project_id: Uuid,
    pub user_id: String,
    pub invoice_id: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_automation_rules)]
pub struct ProjectAutomationRule {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub trigger: String,
    pub conditions: Value,
    pub actions: Value,
    pub enabled: bool,
    pub run_count: i64,
    pub last_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_automation_rules)]
pub struct NewProjectAutomationRule {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub trigger: String,
    pub last_run_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_templates)]
pub struct ProjectTemplate {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub methodology: String,
    pub boards: Value,
    pub columns: Value,
    pub statuses: Value,
    pub phases: Value,
    pub custom_fields: Value,
    pub is_system: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_templates)]
pub struct NewProjectTemplate {
    pub id: Uuid,
    pub name: String,
    pub methodology: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_activity)]
pub struct ProjectActivity {
    pub id: Uuid,
    pub project_id: Uuid,
    pub actor_id: String,
    pub action: String,
    pub target_type: String,
    pub target_id: Option<String>,
    pub details: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_activity)]
pub struct NewProjectActivity {
    pub id: Uuid,
    pub project_id: Uuid,
    pub actor_id: String,
    pub action: String,
    pub target_type: String,
    pub target_id: Option<String>,
}

// ---------------------------------------------------------------------------
// TimeTracker (migration 000032_timetracker; batch 2 notes §4).
// Field names/types mirror the in-module structs in timetracker.rs.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timetracker_timers)]
pub struct TimeTrackerTimer {
    pub id: Uuid,
    pub user_id: String,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub description: Option<String>,
    pub billable: bool,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub paused_total_secs: i64,
    pub paused_at: Option<DateTime<Utc>>,
    pub idle_total_secs: i64,
    pub stopped_at: Option<DateTime<Utc>>,
    pub entry_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timetracker_timers)]
pub struct NewTimeTrackerTimer {
    pub id: Uuid,
    pub user_id: String,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub description: Option<String>,
    pub status: String,
    pub paused_at: Option<DateTime<Utc>>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub entry_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timetracker_entries)]
pub struct TimeEntry {
    pub id: Uuid,
    pub user_id: String,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub timer_id: Option<Uuid>,
    pub description: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub duration_secs: i64,
    pub billable: bool,
    pub approval: String,
    pub approved_by: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub rejection_note: Option<String>,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timetracker_entries)]
pub struct NewTimeEntry {
    pub id: Uuid,
    pub user_id: String,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub timer_id: Option<Uuid>,
    pub description: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub duration_secs: i64,
    pub approved_by: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub rejection_note: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timetracker_activity)]
pub struct ActivitySample {
    pub id: Uuid,
    pub user_id: String,
    pub timer_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub activity_score: i32,
    pub idle_secs: i64,
    pub interval_start: DateTime<Utc>,
    pub interval_end: DateTime<Utc>,
    pub triggered_auto_pause: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timetracker_activity)]
pub struct NewActivitySample {
    pub id: Uuid,
    pub user_id: String,
    pub timer_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub activity_score: i32,
    pub interval_start: DateTime<Utc>,
    pub interval_end: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timetracker_app_usage)]
pub struct AppUsageRecord {
    pub id: Uuid,
    pub user_id: String,
    pub application_name: String,
    pub domain: Option<String>,
    pub window_title_hash: Option<String>,
    pub category: String,
    pub duration_secs: i64,
    pub interval_start: DateTime<Utc>,
    pub interval_end: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timetracker_app_usage)]
pub struct NewAppUsageRecord {
    pub id: Uuid,
    pub user_id: String,
    pub application_name: String,
    pub domain: Option<String>,
    pub window_title_hash: Option<String>,
    pub duration_secs: i64,
    pub interval_start: DateTime<Utc>,
    pub interval_end: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timetracker_screenshots)]
pub struct ScreenshotRecord {
    pub id: Uuid,
    pub user_id: String,
    pub timer_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub storage_target: String,
    pub storage_key: String,
    pub content_sha256: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub blurred: bool,
    pub captured_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timetracker_screenshots)]
pub struct NewScreenshotRecord {
    pub id: Uuid,
    pub user_id: String,
    pub timer_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub storage_target: String,
    pub storage_key: String,
    pub content_sha256: String,
    pub content_type: String,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = timetracker_rates)]
pub struct HourlyRate {
    pub id: Uuid,
    pub user_id: String,
    pub currency: String,
    pub rate: PgNumeric,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = timetracker_rates)]
pub struct NewHourlyRate {
    pub id: Uuid,
    pub user_id: String,
    pub currency: String,
    pub rate: PgNumeric,
    pub effective_to: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timetracker_timesheets)]
pub struct Timesheet {
    pub id: Uuid,
    pub user_id: String,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub status: String,
    pub entry_ids: Vec<Uuid>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub review_note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timetracker_timesheets)]
pub struct NewTimesheet {
    pub id: Uuid,
    pub user_id: String,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub review_note: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = timetracker_settings)]
pub struct TrackerSettings {
    pub id: Uuid,
    pub scope: String,
    pub idle_threshold_secs: i64,
    pub retention_days: i64,
    pub overtime_hours_per_week: PgNumeric,
    pub overtime_multiplier: PgNumeric,
    pub low_activity_threshold: i64,
    pub storage_target: String,
    pub default_currency: String,
    pub auto_approve_timer_entries: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = timetracker_settings)]
pub struct NewTrackerSettings {
    pub id: Uuid,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timetracker_user_settings)]
pub struct UserTrackerSettings {
    pub id: Uuid,
    pub user_id: String,
    pub screenshots_enabled: bool,
    pub blur_screenshots: bool,
    pub idle_threshold_secs: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timetracker_user_settings)]
pub struct NewUserTrackerSettings {
    pub id: Uuid,
    pub user_id: String,
    pub idle_threshold_secs: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timetracker_teams)]
pub struct Team {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub manager_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timetracker_teams)]
pub struct NewTeam {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub manager_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timetracker_team_members)]
pub struct TeamMember {
    pub id: Uuid,
    pub team_id: Uuid,
    pub user_id: String,
    pub role: String,
    pub joined_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timetracker_team_members)]
pub struct NewTeamMember {
    pub id: Uuid,
    pub team_id: Uuid,
    pub user_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timetracker_staff_settings)]
pub struct StaffSettings {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Uuid,
    pub role: String,
    pub manager_id: Option<String>,
    pub screenshots_enabled: Option<bool>,
    pub blur_screenshots: Option<bool>,
    pub idle_threshold_secs: Option<i64>,
    pub activity_level: Option<String>,
    pub timezone: Option<String>,
    pub working_hours_start: Option<String>,
    pub working_hours_end: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timetracker_staff_settings)]
pub struct NewStaffSettings {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Uuid,
    pub manager_id: Option<String>,
    pub screenshots_enabled: Option<bool>,
    pub blur_screenshots: Option<bool>,
    pub idle_threshold_secs: Option<i64>,
    pub activity_level: Option<String>,
    pub timezone: Option<String>,
    pub working_hours_start: Option<String>,
    pub working_hours_end: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timetracker_staff_invites)]
pub struct StaffInvite {
    pub id: Uuid,
    pub company_id: Uuid,
    pub email: String,
    pub role: String,
    pub team_id: Option<Uuid>,
    pub invited_by: String,
    pub token_hash: String,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub accepted_user_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timetracker_staff_invites)]
pub struct NewStaffInvite {
    pub id: Uuid,
    pub company_id: Uuid,
    pub email: String,
    pub team_id: Option<Uuid>,
    pub invited_by: String,
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub accepted_user_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = timetracker_buyer_settings)]
pub struct BuyerSettings {
    pub id: Uuid,
    pub company_id: Uuid,
    pub screenshots_enabled: Option<bool>,
    pub screenshot_interval_secs: Option<i64>,
    pub activity_level: Option<String>,
    pub idle_timeout_secs: Option<i64>,
    pub blur_screenshots: Option<bool>,
    pub allowed_apps: Vec<String>,
    pub blocked_apps: Vec<String>,
    pub allowed_websites: Vec<String>,
    pub blocked_websites: Vec<String>,
    pub default_hourly_rate: Option<PgNumeric>,
    pub timezone: Option<String>,
    pub working_hours_start: Option<String>,
    pub working_hours_end: Option<String>,
    pub working_days: Vec<i64>,
    pub retention_days: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = timetracker_buyer_settings)]
pub struct NewBuyerSettings {
    pub id: Uuid,
    pub company_id: Uuid,
    pub screenshots_enabled: Option<bool>,
    pub screenshot_interval_secs: Option<i64>,
    pub activity_level: Option<String>,
    pub idle_timeout_secs: Option<i64>,
    pub blur_screenshots: Option<bool>,
    pub default_hourly_rate: Option<PgNumeric>,
    pub timezone: Option<String>,
    pub working_hours_start: Option<String>,
    pub working_hours_end: Option<String>,
    pub retention_days: Option<i64>,
}

// ---------------------------------------------------------------------------
// Storage manager (migration 000033_storage_manager; batch 2 notes §5).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_buckets)]
pub struct StorageBucket {
    pub id: Uuid,
    pub name: String,
    pub provider_kind: String,
    pub endpoint: String,
    pub region: String,
    pub container: String,
    pub path: String,
    pub sftp_host: String,
    pub sftp_port: i32,
    pub sftp_username: String,
    pub sftp_auth_kind: String,
    pub sftp_keyfile: String,
    pub allowed_hosts: Vec<String>,
    #[serde(skip_serializing)]
    pub credentials_encrypted: Option<String>,
    pub key_fingerprint: Option<String>,
    pub encryption_at_rest: bool,
    pub enabled: bool,
    pub company_id: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_buckets)]
pub struct NewStorageBucket {
    pub id: Uuid,
    pub name: String,
    pub provider_kind: String,
    #[serde(skip_serializing)]
    pub credentials_encrypted: Option<String>,
    pub key_fingerprint: Option<String>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_bindings)]
pub struct StorageBinding {
    pub id: Uuid,
    pub module_id: String,
    pub bucket_id: Uuid,
    pub key_prefix: String,
    pub company_id: Option<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_bindings)]
pub struct NewStorageBinding {
    pub id: Uuid,
    pub module_id: String,
    pub bucket_id: Uuid,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_quotas)]
pub struct StorageQuota {
    pub id: Uuid,
    pub scope: String,
    pub scope_id: String,
    pub bytes_limit: Option<i64>,
    pub objects_limit: Option<i64>,
    pub soft_bytes_limit: Option<i64>,
    pub soft_objects_limit: Option<i64>,
    pub used_bytes: i64,
    pub used_objects: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_quotas)]
pub struct NewStorageQuota {
    pub id: Uuid,
    pub scope: String,
    pub scope_id: String,
    pub bytes_limit: Option<i64>,
    pub objects_limit: Option<i64>,
    pub soft_bytes_limit: Option<i64>,
    pub soft_objects_limit: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_usage_daily)]
pub struct UsageDaily {
    pub id: Uuid,
    pub date: NaiveDate,
    pub bucket_id: Uuid,
    pub company_id: Option<String>,
    pub user_id: String,
    pub module_id: String,
    pub bytes_total: i64,
    pub objects_total: i64,
    pub bytes_delta: i64,
    pub objects_delta: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_usage_daily)]
pub struct NewUsageDaily {
    pub id: Uuid,
    pub date: NaiveDate,
    pub bucket_id: Uuid,
    pub company_id: Option<String>,
    pub user_id: String,
    pub module_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_lifecycle_rules)]
pub struct LifecycleRule {
    pub id: Uuid,
    pub bucket_id: Uuid,
    pub retention_days: i64,
    pub auto_delete: bool,
    pub archive_transition: bool,
    pub archive_prefix: String,
    pub enabled: bool,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_run_summary: Option<String>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_lifecycle_rules)]
pub struct NewLifecycleRule {
    pub id: Uuid,
    pub bucket_id: Uuid,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_run_summary: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_signed_url_revocations)]
pub struct SignedUrlRecord {
    pub id: Uuid,
    pub bucket_id: Uuid,
    pub key: String,
    pub issued_by: String,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoke_reason: Option<String>,
    pub provider_url_issued: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_signed_url_revocations)]
pub struct NewSignedUrlRecord {
    pub id: Uuid,
    pub bucket_id: Uuid,
    pub key: String,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoke_reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_health_checks)]
pub struct HealthCheck {
    pub id: Uuid,
    pub bucket_id: Uuid,
    pub ok: bool,
    pub latency_ms: i64,
    pub detail: String,
    pub checked_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_health_checks)]
pub struct NewHealthCheck {
    pub id: Uuid,
    pub bucket_id: Uuid,
    pub ok: bool,
}

// ---------------------------------------------------------------------------
// Events (migration 000034_events; batch 2 notes §6). `New*` structs
// added per the notes' convention (the notes give Queryable only).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = events)]
pub struct EventRow {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub slug: String,
    pub category: Vec<String>,
    pub tags: Vec<String>,
    pub venue_name: Option<String>,
    pub venue_address: Option<String>,
    pub venue_latitude: Option<f64>,
    pub venue_longitude: Option<f64>,
    pub venue_capacity: Option<i64>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub timezone: String,
    pub visibility: String,
    pub status: String,
    pub organizer_id: String,
    pub company_id: Option<String>,
    pub cover_storage_target: Option<String>,
    pub cover_storage_key: Option<String>,
    pub recurrence: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = events)]
pub struct NewEventRow {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub slug: String,
    pub venue_name: Option<String>,
    pub venue_address: Option<String>,
    pub venue_latitude: Option<f64>,
    pub venue_longitude: Option<f64>,
    pub venue_capacity: Option<i64>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub organizer_id: String,
    pub company_id: Option<String>,
    pub cover_storage_target: Option<String>,
    pub cover_storage_key: Option<String>,
    pub recurrence: Option<Value>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = event_occurrences)]
pub struct EventOccurrenceRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub sequence: i32,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub status: String,
    pub venue_name: Option<String>,
    pub venue_address: Option<String>,
    pub venue_latitude: Option<f64>,
    pub venue_longitude: Option<f64>,
    pub venue_capacity: Option<i64>,
    pub overridden: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = event_occurrences)]
pub struct NewEventOccurrenceRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub sequence: i32,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub venue_name: Option<String>,
    pub venue_address: Option<String>,
    pub venue_latitude: Option<f64>,
    pub venue_longitude: Option<f64>,
    pub venue_capacity: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = event_ticket_types)]
pub struct EventTicketTypeRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub price: PgNumeric,
    pub currency: String,
    pub quantity: i64,
    pub sold: i64,
    pub sales_start: Option<DateTime<Utc>>,
    pub sales_end: Option<DateTime<Utc>>,
    pub per_order_limit: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = event_ticket_types)]
pub struct NewEventTicketTypeRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub sales_start: Option<DateTime<Utc>>,
    pub sales_end: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = event_registrations)]
pub struct EventRegistrationRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub occurrence_id: Option<Uuid>,
    pub ticket_type_id: Uuid,
    pub user_id: String,
    pub quantity: i64,
    pub total_amount: PgNumeric,
    pub currency: String,
    pub status: String,
    pub order_id: Option<String>,
    pub payment_reference: Option<String>,
    pub payment_provider: Option<String>,
    pub check_in_code: String,
    pub checked_in_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub refund_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = event_registrations)]
pub struct NewEventRegistrationRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub occurrence_id: Option<Uuid>,
    pub ticket_type_id: Uuid,
    pub user_id: String,
    pub order_id: Option<String>,
    pub payment_reference: Option<String>,
    pub payment_provider: Option<String>,
    pub check_in_code: String,
    pub checked_in_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub refund_reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = event_attendees)]
pub struct EventAttendeeRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub registration_id: Uuid,
    pub name: String,
    pub email: String,
    pub custom_answers: Value,
    pub checked_in_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = event_attendees)]
pub struct NewEventAttendeeRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub registration_id: Uuid,
    pub name: String,
    pub email: String,
    pub checked_in_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = event_waitlist)]
pub struct EventWaitlistRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub ticket_type_id: Uuid,
    pub user_id: String,
    pub quantity: i64,
    pub position: i64,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = event_waitlist)]
pub struct NewEventWaitlistRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub ticket_type_id: Uuid,
    pub user_id: String,
    pub position: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = event_reminders)]
pub struct EventReminderRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub offset_minutes: i64,
    pub occurrence_id: Option<Uuid>,
    pub title_template: Option<String>,
    pub body_template: Option<String>,
    pub status: String,
    pub scheduler_job_id: Option<String>,
    pub scheduler_status: String,
    pub sent_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = event_reminders)]
pub struct NewEventReminderRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub offset_minutes: i64,
    pub occurrence_id: Option<Uuid>,
    pub title_template: Option<String>,
    pub body_template: Option<String>,
    pub scheduler_job_id: Option<String>,
    pub sent_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Todos (migration 000035_todos; batch 2 notes §7).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = todo_lists)]
pub struct TodoListRow {
    pub id: Uuid,
    pub owner_id: String,
    pub name: String,
    pub color: String,
    pub icon: String,
    pub archived: bool,
    pub sort_order: i32,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = todo_lists)]
pub struct NewTodoListRow {
    pub id: Uuid,
    pub owner_id: String,
    pub name: String,
    pub color: String,
    pub icon: String,
    pub sort_order: i32,
    pub is_default: bool,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = todos)]
pub struct TodoRow {
    pub id: Uuid,
    pub list_id: Uuid,
    pub owner_id: String,
    pub title: String,
    pub notes: Option<String>,
    pub due_at: Option<DateTime<Utc>>,
    pub timezone: String,
    pub priority: String,
    pub status: String,
    pub tags: Value,
    pub subtasks: Value,
    pub attachments: Value,
    pub recurrence_frequency: Option<String>,
    pub recurrence_interval: i32,
    pub recurrence_count: Option<i32>,
    pub recurrence_until: Option<DateTime<Utc>>,
    pub recurrence_remaining: Option<i32>,
    pub recurrence_group_id: Option<Uuid>,
    pub position: i32,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = todos)]
pub struct NewTodoRow {
    pub id: Uuid,
    pub list_id: Uuid,
    pub owner_id: String,
    pub title: String,
    pub notes: Option<String>,
    pub due_at: Option<DateTime<Utc>>,
    pub timezone: String,
    pub priority: String,
    pub status: String,
    pub tags: Value,
    pub subtasks: Value,
    pub attachments: Value,
    pub recurrence_frequency: Option<String>,
    pub recurrence_interval: i32,
    pub recurrence_count: Option<i32>,
    pub recurrence_until: Option<DateTime<Utc>>,
    pub recurrence_remaining: Option<i32>,
    pub recurrence_group_id: Option<Uuid>,
    pub position: i32,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = todo_subtasks)]
pub struct TodoSubtaskRow {
    pub id: Uuid,
    pub todo_id: Uuid,
    pub title: String,
    pub done: bool,
    pub position: i32,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = todo_subtasks)]
pub struct NewTodoSubtaskRow {
    pub id: Uuid,
    pub todo_id: Uuid,
    pub title: String,
    pub done: bool,
    pub position: i32,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = todo_shares)]
pub struct TodoShareRow {
    pub id: Uuid,
    pub list_id: Uuid,
    pub owner_id: String,
    pub shared_with_user_id: String,
    pub permission: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = todo_shares)]
pub struct NewTodoShareRow {
    pub id: Uuid,
    pub list_id: Uuid,
    pub owner_id: String,
    pub shared_with_user_id: String,
    pub permission: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = todo_completion_log)]
pub struct TodoCompletionLogRow {
    pub id: Uuid,
    pub todo_id: Uuid,
    pub user_id: String,
    pub action: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = todo_completion_log)]
pub struct NewTodoCompletionLogRow {
    pub id: Uuid,
    pub todo_id: Uuid,
    pub user_id: String,
    pub action: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = todo_reminders)]
pub struct TodoReminderRow {
    pub id: Uuid,
    pub todo_id: Uuid,
    pub owner_id: String,
    pub offset_minutes: i64,
    pub remind_at: DateTime<Utc>,
    pub due_at: DateTime<Utc>,
    pub status: String,
    pub scheduler_job_id: Option<String>,
    pub scheduler_status: String,
    pub sent_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = todo_reminders)]
pub struct NewTodoReminderRow {
    pub id: Uuid,
    pub todo_id: Uuid,
    pub owner_id: String,
    pub offset_minutes: i64,
    pub remind_at: DateTime<Utc>,
    pub due_at: DateTime<Utc>,
    pub status: String,
    pub scheduler_job_id: Option<String>,
    pub scheduler_status: String,
    pub sent_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// AI access allowlists (migration 000036_ai_allowlists; batch 2 notes §8).
// Money structs carry no serde derives (PgNumeric has none); the notes'
// `New*` snippets show `Deserialize` on money structs, which cannot
// compile for PgNumeric fields, so those derives are dropped here.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = ai_company_policies)]
pub struct AiCompanyPolicy {
    pub id: Uuid,
    pub company_id: Uuid,
    pub allowed_models: Option<Value>,
    pub allowed_features: Option<Value>,
    pub monthly_token_cap: Option<i64>,
    pub monthly_cost_cap_usd: Option<PgNumeric>,
    pub enabled: bool,
    pub ai_enabled: bool,
    pub disabled_features: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = ai_company_policies)]
pub struct NewAiCompanyPolicy {
    pub id: Uuid,
    pub company_id: Uuid,
    pub allowed_models: Option<Value>,
    pub allowed_features: Option<Value>,
    pub monthly_token_cap: Option<i64>,
    pub monthly_cost_cap_usd: Option<PgNumeric>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = ai_user_policies)]
pub struct AiUserPolicy {
    pub id: Uuid,
    pub user_id: Uuid,
    pub company_id: Uuid,
    pub allowed_models: Option<Value>,
    pub allowed_features: Option<Value>,
    pub monthly_token_cap: Option<i64>,
    pub monthly_cost_cap_usd: Option<PgNumeric>,
    pub enabled: bool,
    pub ai_enabled: bool,
    pub disabled_features: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = ai_user_policies)]
pub struct NewAiUserPolicy {
    pub id: Uuid,
    pub user_id: Uuid,
    pub company_id: Uuid,
    pub allowed_models: Option<Value>,
    pub allowed_features: Option<Value>,
    pub monthly_token_cap: Option<i64>,
    pub monthly_cost_cap_usd: Option<PgNumeric>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = ai_personal_keys)]
pub struct AiPersonalKey {
    pub id: Uuid,
    pub user_id: Uuid,
    pub provider_kind: String,
    pub base_url: Option<String>,
    pub model: Option<String>,
    #[serde(skip_serializing)]
    pub api_key_encrypted: String,
    pub key_fingerprint: String,
    pub monthly_token_cap: Option<i64>,
    pub monthly_cost_cap_usd: Option<PgNumeric>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = ai_personal_keys)]
pub struct NewAiPersonalKey {
    pub id: Uuid,
    pub user_id: Uuid,
    pub provider_kind: String,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key_encrypted: String,
    pub key_fingerprint: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = ai_user_mcp_servers)]
pub struct AiUserMcpServer {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub endpoint_url: String,
    #[serde(skip_serializing)]
    pub auth_token_encrypted: Option<String>,
    pub token_fingerprint: Option<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = ai_user_mcp_servers)]
pub struct NewAiUserMcpServer {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub endpoint_url: String,
    pub auth_token_encrypted: Option<String>,
    pub token_fingerprint: Option<String>,
}

// ---------------------------------------------------------------------------
// Calculator Enterprise (2026-11-22-000100_calculator).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = calculator_saved_calculations)]
pub struct CalculatorSavedCalculation {
    pub id: Uuid,
    pub company_id: Uuid,
    pub calculator_type: String,
    pub title: String,
    pub inputs: Value,
    pub results: Value,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = calculator_saved_calculations)]
pub struct NewCalculatorSavedCalculation {
    pub id: Uuid,
    pub company_id: Uuid,
    pub calculator_type: String,
    pub title: String,
    pub inputs: Value,
    pub results: Value,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = calculator_reports)]
pub struct CalculatorReport {
    pub id: Uuid,
    pub calculation_id: Uuid,
    pub company_id: Uuid,
    pub title: String,
    pub share_token: String,
    pub visibility: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = calculator_reports)]
pub struct NewCalculatorReport {
    pub id: Uuid,
    pub calculation_id: Uuid,
    pub company_id: Uuid,
    pub title: String,
    pub share_token: String,
    pub visibility: String,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = calculator_history)]
pub struct CalculatorHistory {
    pub id: Uuid,
    pub company_id: Uuid,
    pub calculator_type: String,
    pub inputs: Value,
    pub results: Value,
    pub actor: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = calculator_history)]
pub struct NewCalculatorHistory {
    pub id: Uuid,
    pub company_id: Uuid,
    pub calculator_type: String,
    pub inputs: Value,
    pub results: Value,
    pub actor: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = calculator_currency_rates)]
pub struct CalculatorCurrencyRate {
    pub id: Uuid,
    pub base_currency: String,
    pub target_currency: String,
    pub rate: f64,
    pub provider: String,
    pub fetched_at: DateTime<Utc>,
    pub max_age_seconds: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = calculator_currency_rates)]
pub struct NewCalculatorCurrencyRate {
    pub id: Uuid,
    pub base_currency: String,
    pub target_currency: String,
    pub rate: f64,
    pub provider: String,
    pub fetched_at: DateTime<Utc>,
    pub max_age_seconds: i32,
}

// ---------------------------------------------------------------------------
// Human Resource Management (2026-11-22-000037_hr). Money fields are
// `PgNumeric` (NUMERIC(19,4) in SQL); leave days are `PgNumeric`
// (NUMERIC(10,2) in SQL). Money structs carry no serde derives
// (PgNumeric has none) — the notes' snippets show Serialize /
// Deserialize on money structs; those derives are dropped here per
// the repo money convention.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_employees)]
pub struct HrEmployee {
    pub id: Uuid,
    pub user_id: String,
    pub employee_code: String,
    pub position_id: Option<Uuid>,
    pub department_id: Option<String>,
    pub department_verified: bool,
    pub manager_id: Option<Uuid>,
    pub status: String,
    pub hire_date: NaiveDate,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_employees)]
pub struct NewHrEmployee {
    pub id: Uuid,
    pub user_id: String,
    pub employee_code: String,
    pub position_id: Option<Uuid>,
    pub department_id: Option<String>,
    pub department_verified: bool,
    pub manager_id: Option<Uuid>,
    pub status: String,
    pub hire_date: NaiveDate,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = hr_positions)]
pub struct HrPosition {
    pub id: Uuid,
    pub title: String,
    pub department_id: Option<String>,
    pub level: Option<String>,
    pub salary_band_min: Option<PgNumeric>,
    pub salary_band_max: Option<PgNumeric>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = hr_positions)]
pub struct NewHrPosition {
    pub id: Uuid,
    pub title: String,
    pub department_id: Option<String>,
    pub level: Option<String>,
    pub salary_band_min: Option<PgNumeric>,
    pub salary_band_max: Option<PgNumeric>,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = hr_contracts)]
pub struct HrContract {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub contract_type: String,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
    pub salary_amount: PgNumeric,
    pub currency: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = hr_contracts)]
pub struct NewHrContract {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub contract_type: String,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
    pub salary_amount: PgNumeric,
    pub currency: String,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = hr_leave_types)]
pub struct HrLeaveType {
    pub id: Uuid,
    pub name: String,
    pub code: String,
    pub default_days_per_year: PgNumeric,
    pub paid: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = hr_leave_types)]
pub struct NewHrLeaveType {
    pub id: Uuid,
    pub name: String,
    pub code: String,
    pub default_days_per_year: PgNumeric,
    pub paid: bool,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = hr_leave_balances)]
pub struct HrLeaveBalance {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub leave_type_id: Uuid,
    pub year: i32,
    pub entitled_days: PgNumeric,
    pub used_days: PgNumeric,
    pub carried_over_days: PgNumeric,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = hr_leave_balances)]
pub struct NewHrLeaveBalance {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub leave_type_id: Uuid,
    pub year: i32,
    pub entitled_days: PgNumeric,
    pub used_days: PgNumeric,
    pub carried_over_days: PgNumeric,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = hr_leave_requests)]
pub struct HrLeaveRequest {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub leave_type_id: Uuid,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub days: PgNumeric,
    pub status: String,
    pub reason: String,
    pub reviewed_by: Option<String>,
    pub review_note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = hr_leave_requests)]
pub struct NewHrLeaveRequest {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub leave_type_id: Uuid,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub days: PgNumeric,
    pub status: String,
    pub reason: String,
    pub reviewed_by: Option<String>,
    pub review_note: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_attendance_records)]
pub struct HrAttendanceRecord {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub user_id: String,
    pub work_date: NaiveDate,
    pub minutes_worked: i32,
    pub source: String,
    pub entry_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_attendance_records)]
pub struct NewHrAttendanceRecord {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub user_id: String,
    pub work_date: NaiveDate,
    pub minutes_worked: i32,
    pub source: String,
    pub entry_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = hr_payroll_runs)]
pub struct HrPayrollRun {
    pub id: Uuid,
    pub period: String,
    pub currency: String,
    pub status: String,
    pub total_gross: PgNumeric,
    pub total_net: PgNumeric,
    pub total_employees: i32,
    pub finalized_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = hr_payroll_runs)]
pub struct NewHrPayrollRun {
    pub id: Uuid,
    pub period: String,
    pub currency: String,
    pub status: String,
    pub total_gross: PgNumeric,
    pub total_net: PgNumeric,
    pub total_employees: i32,
    pub finalized_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = hr_payroll_items)]
pub struct HrPayrollItem {
    pub id: Uuid,
    pub payroll_run_id: Uuid,
    pub employee_id: Uuid,
    pub gross: PgNumeric,
    pub adjustments: PgNumeric,
    pub deductions: PgNumeric,
    pub net: PgNumeric,
    pub notes: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = hr_payroll_items)]
pub struct NewHrPayrollItem {
    pub id: Uuid,
    pub payroll_run_id: Uuid,
    pub employee_id: Uuid,
    pub gross: PgNumeric,
    pub adjustments: PgNumeric,
    pub deductions: PgNumeric,
    pub net: PgNumeric,
    pub notes: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_review_cycles)]
pub struct HrReviewCycle {
    pub id: Uuid,
    pub name: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_review_cycles)]
pub struct NewHrReviewCycle {
    pub id: Uuid,
    pub name: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_reviews)]
pub struct HrReview {
    pub id: Uuid,
    pub cycle_id: Uuid,
    pub employee_id: Uuid,
    pub reviewer_id: String,
    pub rating: Option<i16>,
    pub goals: Value,
    pub reviewer_notes: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_reviews)]
pub struct NewHrReview {
    pub id: Uuid,
    pub cycle_id: Uuid,
    pub employee_id: Uuid,
    pub reviewer_id: String,
    pub rating: Option<i16>,
    pub goals: Value,
    pub reviewer_notes: String,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_checklists)]
pub struct HrChecklist {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub kind: String,
    pub status: String,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_checklists)]
pub struct NewHrChecklist {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub kind: String,
    pub status: String,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_checklist_items)]
pub struct HrChecklistItem {
    pub id: Uuid,
    pub checklist_id: Uuid,
    pub title: String,
    pub completed: bool,
    pub completed_by: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_checklist_items)]
pub struct NewHrChecklistItem {
    pub id: Uuid,
    pub checklist_id: Uuid,
    pub title: String,
    pub completed: bool,
    pub completed_by: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_documents)]
pub struct HrDocument {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub title: String,
    pub document_type: String,
    pub storage_file_id: String,
    pub mime_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub uploaded_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_documents)]
pub struct NewHrDocument {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub title: String,
    pub document_type: String,
    pub storage_file_id: String,
    pub mime_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub uploaded_by: String,
}

// ---------------------------------------------------------------------------
// Tax Management (2026-11-22-000038_tax). Money structs carry no serde
// derives (PgNumeric has none); the notes' `New*` snippets show
// `Deserialize` on money structs — dropped here per the repo money
// convention.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = tax_rules)]
pub struct TaxRule {
    pub id: Uuid,
    pub company_id: Uuid,
    pub country_code: String,
    pub region_code: Option<String>,
    pub name: String,
    pub tax_type: String,
    pub rate_percent: PgNumeric,
    pub threshold_amount: Option<PgNumeric>,
    pub currency: String,
    pub inclusive: bool,
    pub compound: bool,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub sort_order: i32,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = tax_rules)]
pub struct NewTaxRule {
    pub id: Uuid,
    pub company_id: Uuid,
    pub country_code: String,
    pub region_code: Option<String>,
    pub name: String,
    pub tax_type: String,
    pub rate_percent: PgNumeric,
    pub threshold_amount: Option<PgNumeric>,
    pub currency: String,
    pub inclusive: bool,
    pub compound: bool,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub sort_order: i32,
    pub enabled: bool,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = tax_settings)]
pub struct TaxSettings {
    pub id: Uuid,
    pub company_id: Uuid,
    pub fiscal_year_start_month: i32,
    pub fiscal_year_start_day: i32,
    pub filing_frequency: String,
    pub default_currency: String,
    pub default_country_code: Option<String>,
    pub default_region_code: Option<String>,
    pub reminder_days_before: i32,
    pub filing_due_days_after_period: i32,
    pub notify_user_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = tax_settings)]
pub struct NewTaxSettings {
    pub id: Uuid,
    pub company_id: Uuid,
    pub fiscal_year_start_month: i32,
    pub fiscal_year_start_day: i32,
    pub filing_frequency: String,
    pub default_currency: String,
    pub default_country_code: Option<String>,
    pub default_region_code: Option<String>,
    pub reminder_days_before: i32,
    pub filing_due_days_after_period: i32,
    pub notify_user_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = tax_computations)]
pub struct TaxComputation {
    pub id: Uuid,
    pub company_id: Uuid,
    pub source_type: String,
    pub source_id: String,
    pub country_code: Option<String>,
    pub region_code: Option<String>,
    pub currency: String,
    pub base_amount: PgNumeric,
    pub tax_total: PgNumeric,
    pub grand_total: PgNumeric,
    pub lines: Value,
    pub computed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = tax_computations)]
pub struct NewTaxComputation {
    pub id: Uuid,
    pub company_id: Uuid,
    pub source_type: String,
    pub source_id: String,
    pub country_code: Option<String>,
    pub region_code: Option<String>,
    pub currency: String,
    pub base_amount: PgNumeric,
    pub tax_total: PgNumeric,
    pub grand_total: PgNumeric,
    pub lines: Value,
    pub computed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = tax_filings)]
pub struct TaxFiling {
    pub id: Uuid,
    pub company_id: Uuid,
    pub country_code: String,
    pub region_code: Option<String>,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub frequency: String,
    pub due_date: NaiveDate,
    pub status: String,
    pub tax_total: PgNumeric,
    pub submitted_at: Option<DateTime<Utc>>,
    pub submitted_via: Option<String>,
    pub reference: Option<String>,
    pub last_reminder_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = tax_filings)]
pub struct NewTaxFiling {
    pub id: Uuid,
    pub company_id: Uuid,
    pub country_code: String,
    pub region_code: Option<String>,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub frequency: String,
    pub due_date: NaiveDate,
    pub status: String,
    pub tax_total: PgNumeric,
    pub submitted_at: Option<DateTime<Utc>>,
    pub submitted_via: Option<String>,
    pub reference: Option<String>,
    pub last_reminder_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = tax_connectors)]
pub struct TaxConnector {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub kind: String,
    pub endpoint_url: String,
    pub allowed_hosts: Value,
    pub auth_scheme: String,
    #[serde(skip_serializing)]
    pub credential_encrypted: Option<String>,
    pub credential_fingerprint: Option<String>,
    #[serde(skip_serializing)]
    pub signing_secret_encrypted: Option<String>,
    pub signing_secret_fingerprint: Option<String>,
    pub timeout_ms: i32,
    pub max_retries: i32,
    pub enabled: bool,
    pub request_mapping: Value,
    pub response_mapping: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = tax_connectors)]
pub struct NewTaxConnector {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub kind: String,
    pub endpoint_url: String,
    pub allowed_hosts: Value,
    pub auth_scheme: String,
    pub credential_encrypted: Option<String>,
    pub credential_fingerprint: Option<String>,
    pub signing_secret_encrypted: Option<String>,
    pub signing_secret_fingerprint: Option<String>,
    pub timeout_ms: i32,
    pub max_retries: i32,
    pub enabled: bool,
    pub request_mapping: Value,
    pub response_mapping: Value,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = tax_audit)]
pub struct TaxAuditEntry {
    pub id: Uuid,
    pub company_id: Uuid,
    pub actor: String,
    pub action: String,
    pub resource_type: String,
    pub resource_id: String,
    pub details: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = tax_audit)]
pub struct NewTaxAuditEntry {
    pub id: Uuid,
    pub company_id: Uuid,
    pub actor: String,
    pub action: String,
    pub resource_type: String,
    pub resource_id: String,
    pub details: Value,
}

// ---------------------------------------------------------------------------
// Advisor + Risk (2026-11-22-000039_advisor_risk). The notes' structs
// omit `Selectable` and cite a `check_for_backend` convention that
// does not exist in this file (0 occurrences); normalised to the
// file's `Queryable, Selectable, Serialize` / `Insertable,
// Deserialize` convention.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = advisor_suggestions)]
pub struct AdvisorSuggestion {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub rule_id: String,
    pub title: String,
    pub body: String,
    pub recommended_action: String,
    pub severity: String,
    pub severity_reason: String,
    pub status: String,
    pub source_type: String,
    pub source_id: String,
    pub source_ids: Value,
    pub figures: Value,
    pub dedupe_key: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub acknowledged_by: Option<String>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub dismissed_by: Option<String>,
    pub dismissed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = advisor_suggestions)]
pub struct NewAdvisorSuggestion {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub rule_id: String,
    pub title: String,
    pub body: String,
    pub recommended_action: String,
    pub severity: String,
    pub severity_reason: String,
    pub status: String,
    pub source_type: String,
    pub source_id: String,
    pub source_ids: Value,
    pub figures: Value,
    pub dedupe_key: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub acknowledged_by: Option<String>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub dismissed_by: Option<String>,
    pub dismissed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = advisor_rule_runs)]
pub struct AdvisorRuleRun {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub rule_id: String,
    pub ran_at: DateTime<Utc>,
    pub suggestions_created: i32,
    pub suggestions_updated: i32,
    pub inputs_summary: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = advisor_rule_runs)]
pub struct NewAdvisorRuleRun {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub rule_id: String,
    pub ran_at: DateTime<Utc>,
    pub suggestions_created: i32,
    pub suggestions_updated: i32,
    pub inputs_summary: Value,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = risks)]
pub struct Risk {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub title: String,
    pub description: String,
    pub category: String,
    pub likelihood: i16,
    pub impact: i16,
    pub score: i16,
    pub band: String,
    pub owner_id: String,
    pub status: String,
    pub next_review_at: Option<DateTime<Utc>>,
    pub alert_threshold: i16,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risks)]
pub struct NewRisk {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub title: String,
    pub description: String,
    pub category: String,
    pub likelihood: i16,
    pub impact: i16,
    pub score: i16,
    pub band: String,
    pub owner_id: String,
    pub status: String,
    pub next_review_at: Option<DateTime<Utc>>,
    pub alert_threshold: i16,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = risk_mitigation_plans)]
pub struct RiskMitigationPlan {
    pub id: Uuid,
    pub risk_id: Uuid,
    pub title: String,
    pub description: String,
    pub target_date: Option<DateTime<Utc>>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risk_mitigation_plans)]
pub struct NewRiskMitigationPlan {
    pub id: Uuid,
    pub risk_id: Uuid,
    pub title: String,
    pub description: String,
    pub target_date: Option<DateTime<Utc>>,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = risk_mitigation_tasks)]
pub struct RiskMitigationTask {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub risk_id: Uuid,
    pub title: String,
    pub description: String,
    pub assignee_id: Option<String>,
    pub status: String,
    pub due_date: Option<DateTime<Utc>>,
    pub external_project_task_id: Option<String>,
    pub awaiting_projects_handler: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risk_mitigation_tasks)]
pub struct NewRiskMitigationTask {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub risk_id: Uuid,
    pub title: String,
    pub description: String,
    pub assignee_id: Option<String>,
    pub status: String,
    pub due_date: Option<DateTime<Utc>>,
    pub external_project_task_id: Option<String>,
    pub awaiting_projects_handler: bool,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = risk_checklist_templates)]
pub struct RiskChecklistTemplate {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub category: String,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risk_checklist_templates)]
pub struct NewRiskChecklistTemplate {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub category: String,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = risk_checklist_template_items)]
pub struct RiskChecklistTemplateItem {
    pub id: Uuid,
    pub template_id: Uuid,
    pub title: String,
    pub description: String,
    pub required: bool,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risk_checklist_template_items)]
pub struct NewRiskChecklistTemplateItem {
    pub id: Uuid,
    pub template_id: Uuid,
    pub title: String,
    pub description: String,
    pub required: bool,
    pub sort_order: i32,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = risk_checklist_instances)]
pub struct RiskChecklistInstance {
    pub id: Uuid,
    pub template_id: Uuid,
    pub company_id: Option<Uuid>,
    pub risk_id: Option<Uuid>,
    pub title: String,
    pub status: String,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risk_checklist_instances)]
pub struct NewRiskChecklistInstance {
    pub id: Uuid,
    pub template_id: Uuid,
    pub company_id: Option<Uuid>,
    pub risk_id: Option<Uuid>,
    pub title: String,
    pub status: String,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = risk_checklist_instance_items)]
pub struct RiskChecklistInstanceItem {
    pub id: Uuid,
    pub instance_id: Uuid,
    pub template_item_id: Uuid,
    pub title: String,
    pub required: bool,
    pub sort_order: i32,
    pub completed: bool,
    pub evidence: String,
    pub completed_by: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risk_checklist_instance_items)]
pub struct NewRiskChecklistInstanceItem {
    pub id: Uuid,
    pub instance_id: Uuid,
    pub template_item_id: Uuid,
    pub title: String,
    pub required: bool,
    pub sort_order: i32,
    pub completed: bool,
    pub evidence: String,
    pub completed_by: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = risk_alerts)]
pub struct RiskAlert {
    pub id: Uuid,
    pub risk_id: Uuid,
    pub company_id: Option<Uuid>,
    pub kind: String,
    pub message: String,
    pub score: Option<i16>,
    pub notified_user_id: Option<String>,
    pub figures: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = risk_alerts)]
pub struct NewRiskAlert {
    pub id: Uuid,
    pub risk_id: Uuid,
    pub company_id: Option<Uuid>,
    pub kind: String,
    pub message: String,
    pub score: Option<i16>,
    pub notified_user_id: Option<String>,
    pub figures: Value,
}

// ---------------------------------------------------------------------------
// Subscriptions (2026-11-22-000040_subscriptions). `NewSubscription`
// deliberately includes the defaulted business fields (see the notes:
// the writer must persist the values the store computed at
// activation; the SQL defaults would be wrong data). Money structs
// carry no serde derives (PgNumeric has none); the notes' `New*`
// snippet shows `Deserialize` on the money struct — dropped here.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = subscriptions)]
pub struct Subscription {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub product_id: String,
    pub tier_id: Option<String>,
    pub order_id: String,
    pub module_id: Option<String>,
    pub status: String,
    pub billing_period: String,
    pub price: PgNumeric,
    pub currency: String,
    pub trial_days: i32,
    pub trial_end: Option<DateTime<Utc>>,
    pub current_period_start: DateTime<Utc>,
    pub current_period_end: DateTime<Utc>,
    pub proration_policy: String,
    pub grace_period_days: i32,
    pub dunning_max_retries: i32,
    pub dunning_retry_interval_days: i32,
    pub cancel_at_period_end: bool,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub pending_product_id: Option<String>,
    pub pending_tier_id: Option<String>,
    pub pending_price: Option<PgNumeric>,
    pub pending_billing_period: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = subscriptions)]
pub struct NewSubscription {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub product_id: String,
    pub tier_id: Option<String>,
    pub order_id: String,
    pub module_id: Option<String>,
    pub status: String,
    pub billing_period: String,
    pub price: PgNumeric,
    pub currency: String,
    pub trial_days: i32,
    pub trial_end: Option<DateTime<Utc>>,
    pub current_period_start: DateTime<Utc>,
    pub current_period_end: DateTime<Utc>,
    pub proration_policy: String,
    pub grace_period_days: i32,
    pub dunning_max_retries: i32,
    pub dunning_retry_interval_days: i32,
    pub cancel_at_period_end: bool,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub pending_product_id: Option<String>,
    pub pending_tier_id: Option<String>,
    pub pending_price: Option<PgNumeric>,
    pub pending_billing_period: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, Deserialize)]
#[diesel(table_name = subscription_events)]
pub struct SubscriptionEvent {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub event: String,
    pub actor: Option<String>,
    pub reason: Option<String>,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = subscription_events)]
pub struct NewSubscriptionEvent {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub event: String,
    pub actor: Option<String>,
    pub reason: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, Deserialize)]
#[diesel(table_name = dunning_attempts)]
pub struct DunningAttempt {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub attempt_number: i32,
    pub reason: String,
    pub amount_minor: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = dunning_attempts)]
pub struct NewDunningAttempt {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub attempt_number: i32,
    pub reason: String,
    pub amount_minor: Option<i64>,
}

// ---------------------------------------------------------------------------
// Pricing Tables + Marketplace Builder (2026-11-22-000041_pricing_marketplace).
// Money structs carry no serde derives (PgNumeric has none); the
// notes' snippets show Serialize / Deserialize on money structs —
// dropped here per the repo money convention.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pricing_tables)]
pub struct PricingTable {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub currency: String,
    pub layout: String,
    pub theme_accent: String,
    pub status: String,
    pub yearly_discount_percent: PgNumeric,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pricing_tables)]
pub struct NewPricingTable {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub currency: String,
    pub layout: String,
    pub theme_accent: String,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pricing_tiers)]
pub struct PricingTier {
    pub id: Uuid,
    pub table_id: Uuid,
    pub name: String,
    pub description: String,
    pub monthly_price: PgNumeric,
    pub yearly_price: PgNumeric,
    pub cta_label: String,
    pub cta_url: String,
    pub highlighted: bool,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pricing_tiers)]
pub struct NewPricingTier {
    pub id: Uuid,
    pub table_id: Uuid,
    pub name: String,
    pub monthly_price: PgNumeric,
    pub yearly_price: PgNumeric,
    pub cta_label: String,
    pub cta_url: String,
    pub highlighted: bool,
    pub sort_order: i32,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = pricing_tier_features)]
pub struct PricingTierFeature {
    pub id: Uuid,
    pub tier_id: Uuid,
    pub feature_text: String,
    pub included: bool,
    pub note: String,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = pricing_tier_features)]
pub struct NewPricingTierFeature {
    pub id: Uuid,
    pub tier_id: Uuid,
    pub feature_text: String,
    pub included: bool,
    pub sort_order: i32,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketplace_vendors)]
pub struct MarketplaceVendor {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<String>,
    pub company_name: String,
    pub slug: String,
    pub email: String,
    pub status: String,
    pub application_notes: String,
    pub review_reason: String,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketplace_vendors)]
pub struct NewMarketplaceVendor {
    pub id: Uuid,
    pub user_id: String,
    pub company_name: String,
    pub slug: String,
    pub email: String,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = marketplace_commission_rules)]
pub struct MarketplaceCommissionRule {
    pub id: Uuid,
    pub scope: String,
    pub scope_value: String,
    pub rate_percent: PgNumeric,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = marketplace_commission_rules)]
pub struct NewMarketplaceCommissionRule {
    pub id: Uuid,
    pub scope: String,
    pub scope_value: String,
    pub rate_percent: PgNumeric,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketplace_product_approvals)]
pub struct MarketplaceProductApproval {
    pub id: Uuid,
    pub product_id: String,
    pub vendor_id: Uuid,
    pub category: Option<String>,
    pub status: String,
    pub reason: String,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketplace_product_approvals)]
pub struct NewMarketplaceProductApproval {
    pub id: Uuid,
    pub product_id: String,
    pub vendor_id: Uuid,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = marketplace_commissions)]
pub struct MarketplaceCommission {
    pub id: Uuid,
    pub order_id: String,
    pub vendor_id: Uuid,
    pub product_id: String,
    pub order_total: PgNumeric,
    pub rate_percent: PgNumeric,
    pub commission_amount: PgNumeric,
    pub currency: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = marketplace_commissions)]
pub struct NewMarketplaceCommission {
    pub id: Uuid,
    pub order_id: String,
    pub vendor_id: Uuid,
    pub product_id: String,
    pub order_total: PgNumeric,
    pub rate_percent: PgNumeric,
    pub commission_amount: PgNumeric,
    pub currency: String,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = marketplace_payouts)]
pub struct MarketplacePayout {
    pub id: Uuid,
    pub vendor_id: Uuid,
    pub amount: PgNumeric,
    pub currency: String,
    pub status: String,
    pub invoice_id: Option<String>,
    pub reference: String,
    pub requested_by: String,
    pub reviewed_by: Option<String>,
    pub review_reason: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = marketplace_payouts)]
pub struct NewMarketplacePayout {
    pub id: Uuid,
    pub vendor_id: Uuid,
    pub amount: PgNumeric,
    pub currency: String,
    pub status: String,
    pub requested_by: String,
}

// ---------------------------------------------------------------------------
// Sharing / auditors (migration 000068_sharing_auditor; batch 5 notes §d).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = public_links)]
pub struct PublicLinkRow {
    pub id: Uuid,
    pub token_hash: String,
    pub scope_kind: String,
    pub scope_id: Uuid,
    pub scope_view: String,
    pub created_by: String,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = public_links)]
pub struct NewPublicLinkRow {
    pub id: Uuid,
    pub token_hash: String,
    pub scope_kind: String,
    pub scope_id: Uuid,
    pub scope_view: String,
    pub created_by: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = public_link_access_log)]
pub struct PublicLinkAccessLogRow {
    pub id: Uuid,
    pub link_id: Option<Uuid>,
    pub accessed_at: DateTime<Utc>,
    pub ip_hash: Option<String>,
    pub user_agent_hash: Option<String>,
    pub allowed: bool,
    pub reason: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = public_link_access_log)]
pub struct NewPublicLinkAccessLogRow {
    pub id: Uuid,
    pub link_id: Option<Uuid>,
    pub ip_hash: Option<String>,
    pub user_agent_hash: Option<String>,
    pub allowed: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = auditor_accounts)]
pub struct AuditorAccountRow {
    pub id: Uuid,
    pub user_id: String,
    pub label: String,
    pub email: String,
    pub is_auditor: bool,
    pub auditor_of: String,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<String>,
    pub revoke_reason: Option<String>,
    pub scheduler_status: String,
    pub scheduler_job_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = auditor_accounts)]
pub struct NewAuditorAccountRow {
    pub id: Uuid,
    pub user_id: String,
    pub label: String,
    pub email: String,
    pub auditor_of: String,
    pub expires_at: DateTime<Utc>,
    pub password_hash: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = auditor_scopes)]
pub struct AuditorScopeRow {
    pub id: Uuid,
    pub auditor_id: Uuid,
    pub module: String,
    pub resource_kind: String,
    pub resource_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub allow_export: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = auditor_scopes)]
pub struct NewAuditorScopeRow {
    pub id: Uuid,
    pub auditor_id: Uuid,
    pub module: String,
    pub resource_kind: String,
    pub resource_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub allow_export: bool,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = auditor_sessions)]
pub struct AuditorSessionRow {
    pub id: Uuid,
    pub auditor_id: Uuid,
    pub user_id: String,
    pub started_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = auditor_sessions)]
pub struct NewAuditorSessionRow {
    pub id: Uuid,
    pub auditor_id: Uuid,
    pub user_id: String,
}

// ---------------------------------------------------------------------------
// Properties (migration 000061_properties; batch 5 notes §d). The notes
// give Queryable structs only; `New*` structs are added per the file
// convention (id carried, created_at/updated_at and DB-defaulted
// columns omitted). Money structs carry no serde derives.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = properties)]
pub struct PropertyRow {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub slug: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub address: Option<String>,
    pub total_area_sqm: Option<PgNumeric>,
    pub company_id: Option<Uuid>,
    pub status: String,
    pub cover_storage_target: Option<String>,
    pub cover_storage_key: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = properties)]
pub struct NewPropertyRow {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub slug: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub address: Option<String>,
    pub total_area_sqm: Option<PgNumeric>,
    pub company_id: Option<Uuid>,
    pub cover_storage_target: Option<String>,
    pub cover_storage_key: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = property_plans)]
pub struct PropertyPlanRow {
    pub id: Uuid,
    pub property_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub geometry: Option<Value>,
    pub geometry_hash: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = property_plans)]
pub struct NewPropertyPlanRow {
    pub id: Uuid,
    pub property_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub geometry: Option<Value>,
    pub geometry_hash: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = property_blocks)]
pub struct PropertyBlockRow {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub property_id: Uuid,
    pub title: String,
    pub notes: Option<String>,
    pub geometry: Value,
    pub geometry_hash: String,
    pub area_sqm: Option<PgNumeric>,
    pub length_m: Option<PgNumeric>,
    pub price: PgNumeric,
    pub currency: String,
    pub status: String,
    pub assigned_customer_id: Option<String>,
    pub click_behavior: String,
    pub click_link_kind: Option<String>,
    pub click_link_target: Option<String>,
    pub order_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = property_blocks)]
pub struct NewPropertyBlockRow {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub property_id: Uuid,
    pub title: String,
    pub notes: Option<String>,
    pub geometry: Value,
    pub geometry_hash: String,
    pub area_sqm: Option<PgNumeric>,
    pub length_m: Option<PgNumeric>,
    pub assigned_customer_id: Option<String>,
    pub click_link_kind: Option<String>,
    pub click_link_target: Option<String>,
    pub order_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = property_block_history)]
pub struct PropertyBlockHistoryRow {
    pub id: Uuid,
    pub block_id: Uuid,
    pub property_id: Uuid,
    pub plan_id: Uuid,
    pub from_status: String,
    pub to_status: String,
    pub actor_id: String,
    pub reason: Option<String>,
    pub order_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = property_block_history)]
pub struct NewPropertyBlockHistoryRow {
    pub id: Uuid,
    pub block_id: Uuid,
    pub property_id: Uuid,
    pub plan_id: Uuid,
    pub from_status: String,
    pub to_status: String,
    pub actor_id: String,
    pub reason: Option<String>,
    pub order_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Logistics (migration 000062_logistics; batch 5 notes §d). The notes
// give Queryable structs only for most tables and put `Deserialize`
// on Queryable structs; `New*` structs are added per the file
// convention and Queryable structs are normalised to the file's
// `Queryable, Selectable, Serialize` shape. Money structs carry no
// serde derives.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = logistics_drivers)]
pub struct LogisticsDriver {
    pub id: Uuid,
    pub user_id: String,
    pub license_number: String,
    pub phone: Option<String>,
    pub status: String,
    pub current_vehicle_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = logistics_drivers)]
pub struct NewLogisticsDriver {
    pub id: Uuid,
    pub user_id: String,
    pub license_number: String,
    pub phone: Option<String>,
    pub current_vehicle_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = logistics_vehicles)]
pub struct LogisticsVehicle {
    pub id: Uuid,
    pub plate: String,
    pub vehicle_type: String,
    pub capacity_kg: f64,
    pub status: String,
    pub assigned_driver_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub maintenance: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = logistics_vehicles)]
pub struct NewLogisticsVehicle {
    pub id: Uuid,
    pub plate: String,
    pub vehicle_type: String,
    pub capacity_kg: f64,
    pub assigned_driver_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = logistics_warehouses)]
pub struct LogisticsWarehouse {
    pub id: Uuid,
    pub name: String,
    pub address: String,
    pub capacity: i64,
    pub manager_user_id: Option<String>,
    pub default_low_stock_threshold: i64,
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = logistics_warehouses)]
pub struct NewLogisticsWarehouse {
    pub id: Uuid,
    pub name: String,
    pub address: String,
    pub capacity: i64,
    pub manager_user_id: Option<String>,
    pub company_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = logistics_shipments)]
pub struct LogisticsShipment {
    pub id: Uuid,
    pub tracking_number: String,
    #[serde(skip_serializing)]
    pub tracking_token_hash: String,
    pub order_id: Option<String>,
    pub order_link_status: String,
    pub company_id: Option<Uuid>,
    pub status: String,
    pub origin_address: String,
    pub origin_latitude: Option<f64>,
    pub origin_longitude: Option<f64>,
    pub destination_address: String,
    pub destination_latitude: Option<f64>,
    pub destination_longitude: Option<f64>,
    pub weight_kg: f64,
    pub length_cm: f64,
    pub width_cm: f64,
    pub height_cm: f64,
    pub zone_origin: String,
    pub zone_destination: String,
    pub service_level: String,
    pub driver_id: Option<Uuid>,
    pub vehicle_id: Option<Uuid>,
    pub warehouse_id: Option<Uuid>,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub window_start: Option<DateTime<Utc>>,
    pub window_end: Option<DateTime<Utc>>,
    pub estimated_delivery_at: Option<DateTime<Utc>>,
    pub shipping_cost: Option<PgNumeric>,
    pub currency: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = logistics_shipments)]
pub struct NewLogisticsShipment {
    pub id: Uuid,
    pub tracking_number: String,
    pub tracking_token_hash: String,
    pub order_id: Option<String>,
    pub company_id: Option<Uuid>,
    pub origin_address: String,
    pub origin_latitude: Option<f64>,
    pub origin_longitude: Option<f64>,
    pub destination_address: String,
    pub destination_latitude: Option<f64>,
    pub destination_longitude: Option<f64>,
    pub weight_kg: f64,
    pub length_cm: f64,
    pub width_cm: f64,
    pub height_cm: f64,
    pub zone_origin: String,
    pub zone_destination: String,
    pub driver_id: Option<Uuid>,
    pub vehicle_id: Option<Uuid>,
    pub warehouse_id: Option<Uuid>,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub window_start: Option<DateTime<Utc>>,
    pub window_end: Option<DateTime<Utc>>,
    pub estimated_delivery_at: Option<DateTime<Utc>>,
    pub shipping_cost: Option<PgNumeric>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = logistics_shipment_events)]
pub struct LogisticsShipmentEvent {
    pub id: Uuid,
    pub shipment_id: Uuid,
    pub kind: String,
    pub from_status: Option<String>,
    pub to_status: Option<String>,
    pub actor: String,
    pub location: Option<String>,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = logistics_shipment_events)]
pub struct NewLogisticsShipmentEvent {
    pub id: Uuid,
    pub shipment_id: Uuid,
    pub from_status: Option<String>,
    pub to_status: Option<String>,
    pub actor: String,
    pub location: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = logistics_stock_movements)]
pub struct LogisticsStockMovement {
    pub id: Uuid,
    pub warehouse_id: Uuid,
    pub asset_id: Uuid,
    pub direction: String,
    pub quantity: i64,
    pub quantity_after: i64,
    pub reason: String,
    pub shipment_id: Option<Uuid>,
    pub actor: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = logistics_stock_movements)]
pub struct NewLogisticsStockMovement {
    pub id: Uuid,
    pub warehouse_id: Uuid,
    pub asset_id: Uuid,
    pub direction: String,
    pub quantity: i64,
    pub quantity_after: i64,
    pub reason: String,
    pub shipment_id: Option<Uuid>,
    pub actor: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = logistics_routes)]
pub struct LogisticsRoute {
    pub id: Uuid,
    pub name: String,
    pub driver_id: Option<Uuid>,
    pub vehicle_id: Option<Uuid>,
    pub route_date: DateTime<Utc>,
    pub start_latitude: Option<f64>,
    pub start_longitude: Option<f64>,
    pub status: String,
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = logistics_routes)]
pub struct NewLogisticsRoute {
    pub id: Uuid,
    pub name: String,
    pub driver_id: Option<Uuid>,
    pub vehicle_id: Option<Uuid>,
    pub route_date: DateTime<Utc>,
    pub start_latitude: Option<f64>,
    pub start_longitude: Option<f64>,
    pub company_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = logistics_route_stops)]
pub struct LogisticsRouteStop {
    pub id: Uuid,
    pub route_id: Uuid,
    pub shipment_id: Option<Uuid>,
    pub sequence: i64,
    pub address: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub status: String,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = logistics_route_stops)]
pub struct NewLogisticsRouteStop {
    pub id: Uuid,
    pub route_id: Uuid,
    pub shipment_id: Option<Uuid>,
    pub sequence: i64,
    pub address: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = logistics_proofs)]
pub struct LogisticsProof {
    pub id: Uuid,
    pub shipment_id: Uuid,
    pub storage_target: String,
    pub storage_key: String,
    pub content_sha256: String,
    pub signature_storage_target: Option<String>,
    pub signature_storage_key: Option<String>,
    pub recipient_name: String,
    pub delivered_at: DateTime<Utc>,
    pub actor: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = logistics_proofs)]
pub struct NewLogisticsProof {
    pub id: Uuid,
    pub shipment_id: Uuid,
    pub storage_target: String,
    pub storage_key: String,
    pub content_sha256: String,
    pub signature_storage_target: Option<String>,
    pub signature_storage_key: Option<String>,
    pub recipient_name: String,
    pub delivered_at: DateTime<Utc>,
    pub actor: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = logistics_exceptions)]
pub struct LogisticsException {
    pub id: Uuid,
    pub shipment_id: Uuid,
    pub exception_type: String,
    pub description: String,
    pub status: String,
    pub previous_status: String,
    pub resolution: Option<String>,
    pub actor: String,
    pub resolved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = logistics_exceptions)]
pub struct NewLogisticsException {
    pub id: Uuid,
    pub shipment_id: Uuid,
    pub exception_type: String,
    pub description: String,
    pub previous_status: String,
    pub resolution: Option<String>,
    pub actor: String,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = logistics_rate_tables)]
pub struct LogisticsRateTable {
    pub id: Uuid,
    pub name: String,
    pub origin_zone: Option<String>,
    pub destination_zone: Option<String>,
    pub base_price: PgNumeric,
    pub per_kg: PgNumeric,
    pub per_km: PgNumeric,
    pub currency: String,
    pub active: bool,
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = logistics_rate_tables)]
pub struct NewLogisticsRateTable {
    pub id: Uuid,
    pub name: String,
    pub origin_zone: Option<String>,
    pub destination_zone: Option<String>,
    pub base_price: PgNumeric,
    pub per_kg: PgNumeric,
    pub per_km: PgNumeric,
    pub company_id: Option<Uuid>,
}

// ---------------------------------------------------------------------------
// Supervision (migration 000063_supervision; batch 5 notes §d, field
// lists expanded into full structs). Money structs carry no serde
// derives.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = supervision_assignments)]
pub struct SupervisionAssignment {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub user_id: String,
    pub role: String,
    pub status: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = supervision_assignments)]
pub struct NewSupervisionAssignment {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub user_id: String,
    pub role: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = supervision_inspection_templates)]
pub struct SupervisionInspectionTemplate {
    pub id: Uuid,
    pub project_id: Option<String>,
    pub company_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub category: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = supervision_inspection_templates)]
pub struct NewSupervisionInspectionTemplate {
    pub id: Uuid,
    pub project_id: Option<String>,
    pub company_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = supervision_template_items)]
pub struct SupervisionTemplateItem {
    pub id: Uuid,
    pub template_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub position: i32,
    pub required: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = supervision_template_items)]
pub struct NewSupervisionTemplateItem {
    pub id: Uuid,
    pub template_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub position: i32,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = supervision_inspections)]
pub struct SupervisionInspection {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub template_id: Option<Uuid>,
    pub title: String,
    pub location: Option<String>,
    pub inspector_id: String,
    pub status: String,
    pub scheduled_for: DateTime<Utc>,
    pub conducted_at: Option<DateTime<Utc>>,
    pub verdict: Option<String>,
    pub summary: Option<String>,
    pub signed_by: Option<String>,
    pub signed_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = supervision_inspections)]
pub struct NewSupervisionInspection {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub template_id: Option<Uuid>,
    pub title: String,
    pub location: Option<String>,
    pub inspector_id: String,
    pub scheduled_for: DateTime<Utc>,
    pub conducted_at: Option<DateTime<Utc>>,
    pub verdict: Option<String>,
    pub summary: Option<String>,
    pub signed_by: Option<String>,
    pub signed_at: Option<DateTime<Utc>>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = supervision_inspection_items)]
pub struct SupervisionInspectionItem {
    pub id: Uuid,
    pub inspection_id: Uuid,
    pub template_item_id: Option<Uuid>,
    pub title: String,
    pub result: String,
    pub notes: Option<String>,
    pub follow_up_requested: bool,
    pub follow_up_task_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = supervision_inspection_items)]
pub struct NewSupervisionInspectionItem {
    pub id: Uuid,
    pub inspection_id: Uuid,
    pub template_item_id: Option<Uuid>,
    pub title: String,
    pub notes: Option<String>,
    pub follow_up_task_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = supervision_progress_reports)]
pub struct SupervisionProgressReport {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub milestone_id: Option<String>,
    pub title: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub reported_percent_complete: PgNumeric,
    pub planned_percent_complete: Option<PgNumeric>,
    pub project_percent_complete: Option<PgNumeric>,
    pub source: String,
    pub status: String,
    pub notes: Option<String>,
    pub reported_by: String,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = supervision_progress_reports)]
pub struct NewSupervisionProgressReport {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub milestone_id: Option<String>,
    pub title: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub reported_percent_complete: PgNumeric,
    pub planned_percent_complete: Option<PgNumeric>,
    pub project_percent_complete: Option<PgNumeric>,
    pub notes: Option<String>,
    pub reported_by: String,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = supervision_defects)]
pub struct SupervisionDefect {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub inspection_id: Option<Uuid>,
    pub task_id: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub severity: String,
    pub status: String,
    pub reported_by: String,
    pub assigned_to: Option<String>,
    pub follow_up_requested: bool,
    pub follow_up_task_id: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub verified_by: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = supervision_defects)]
pub struct NewSupervisionDefect {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub inspection_id: Option<Uuid>,
    pub task_id: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub reported_by: String,
    pub assigned_to: Option<String>,
    pub follow_up_task_id: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub verified_by: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = supervision_approvals)]
pub struct SupervisionApproval {
    pub id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub approver_id: String,
    pub decision: String,
    pub comment: Option<String>,
    pub decided_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = supervision_approvals)]
pub struct NewSupervisionApproval {
    pub id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub approver_id: String,
    pub decision: String,
    pub comment: Option<String>,
    pub decided_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = supervision_daily_logs)]
pub struct SupervisionDailyLog {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub log_date: NaiveDate,
    pub supervisor_id: String,
    pub weather: Option<String>,
    pub workforce_count: i64,
    pub work_done: Option<String>,
    pub materials_received: Option<String>,
    pub incidents_on_site: Option<String>,
    pub signed_by: Option<String>,
    pub signed_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = supervision_daily_logs)]
pub struct NewSupervisionDailyLog {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub log_date: NaiveDate,
    pub supervisor_id: String,
    pub weather: Option<String>,
    pub work_done: Option<String>,
    pub materials_received: Option<String>,
    pub incidents_on_site: Option<String>,
    pub signed_by: Option<String>,
    pub signed_at: Option<DateTime<Utc>>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = supervision_photos)]
pub struct SupervisionPhoto {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub subject_kind: String,
    pub subject_id: Uuid,
    pub storage_target: String,
    pub storage_key: String,
    pub content_sha256: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub caption: Option<String>,
    pub uploaded_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = supervision_photos)]
pub struct NewSupervisionPhoto {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub subject_kind: String,
    pub subject_id: Uuid,
    pub storage_target: String,
    pub storage_key: String,
    pub content_sha256: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub caption: Option<String>,
    pub uploaded_by: String,
}

// ---------------------------------------------------------------------------
// AI gating (migration 000064_ai_gating; batch 5 notes §c/§d).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = ai_global_settings)]
pub struct AiGlobalSettingsRow {
    pub key: String,
    pub enabled: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = ai_global_settings)]
pub struct NewAiGlobalSettingsRow {
    pub key: String,
    pub enabled: bool,
}

// ---------------------------------------------------------------------------
// Network Tools + Licensing SaaS (batch 7 notes §3). The notes flag
// the convention warning themselves: the network-tools structs
// combine Queryable + Selectable + Insertable on one struct, so they
// are split here into the repo's usual Queryable / New* form
// (table_name form and derives normalised to the file convention;
// the notes' `check_for_backend` is not used in this file).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = network_tool_saved_checks)]
pub struct NetworkToolSavedCheck {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub name: String,
    pub tool: String,
    pub target: String,
    pub parameters: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = network_tool_saved_checks)]
pub struct NewNetworkToolSavedCheck {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub name: String,
    pub tool: String,
    pub target: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = network_tool_runs)]
pub struct NetworkToolRun {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub tool: String,
    pub target: String,
    pub duration_ms: i64,
    pub outcome: String,
    pub summary: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = network_tool_runs)]
pub struct NewNetworkToolRun {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub tool: String,
    pub target: String,
    pub outcome: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = network_tool_uptime_monitors)]
pub struct NetworkToolUptimeMonitor {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub name: String,
    pub url: String,
    pub interval_seconds: i32,
    pub expected_status: i32,
    pub failure_threshold: i32,
    pub consecutive_failures: i32,
    pub enabled: bool,
    pub last_status: Option<String>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = network_tool_uptime_monitors)]
pub struct NewNetworkToolUptimeMonitor {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub name: String,
    pub url: String,
    pub last_status: Option<String>,
    pub last_checked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = network_tool_uptime_events)]
pub struct NetworkToolUptimeEvent {
    pub id: Uuid,
    pub monitor_id: Uuid,
    pub status: String,
    pub http_status: Option<i32>,
    pub latency_ms: Option<i64>,
    pub detail: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = network_tool_uptime_events)]
pub struct NewNetworkToolUptimeEvent {
    pub id: Uuid,
    pub monitor_id: Uuid,
    pub status: String,
    pub http_status: Option<i32>,
    pub latency_ms: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = network_tool_cert_watches)]
pub struct NetworkToolCertWatch {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub domain: String,
    pub enabled: bool,
    pub last_days_remaining: Option<i32>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub alerted_thresholds: Vec<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = network_tool_cert_watches)]
pub struct NewNetworkToolCertWatch {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub domain: String,
    pub last_days_remaining: Option<i32>,
    pub last_checked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = license_products)]
pub struct LicenseProduct {
    pub id: Uuid,
    pub product: String,
    pub display_name: Option<String>,
    pub owner_id: Option<Uuid>,
    pub key_prefix: Option<String>,
    pub segments: i32,
    pub key_format: String,
    pub key_pattern: Option<String>,
    pub default_max_activations: i32,
    pub default_validity_days: Option<i64>,
    pub grace_period_days: i64,
    pub renewal_reminder_days: Vec<i64>,
    pub offline_max_days: i64,
    pub allowed_distribution_channels: Vec<String>,
    pub api_allowed_endpoints: Vec<String>,
    pub api_rate_limit_per_minute: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = license_products)]
pub struct NewLicenseProduct {
    pub id: Uuid,
    pub product: String,
    pub display_name: Option<String>,
    pub owner_id: Option<Uuid>,
    pub key_prefix: Option<String>,
    pub segments: i32,
    pub key_format: String,
    pub key_pattern: Option<String>,
    pub default_max_activations: i32,
    pub default_validity_days: Option<i64>,
    pub grace_period_days: i64,
    pub renewal_reminder_days: Vec<i64>,
    pub offline_max_days: i64,
    pub allowed_distribution_channels: Vec<String>,
    pub api_allowed_endpoints: Vec<String>,
    pub api_rate_limit_per_minute: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = license_distributions)]
pub struct LicenseDistribution {
    pub id: Uuid,
    pub product: String,
    pub provider: String,
    pub repo: String,
    pub git_ref: String,
    pub webhook_secret_hash: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = license_distributions)]
pub struct NewLicenseDistribution {
    pub id: Uuid,
    pub product: String,
    pub provider: String,
    pub repo: String,
    pub git_ref: String,
    pub webhook_secret_hash: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = license_releases)]
pub struct LicenseRelease {
    pub id: Uuid,
    pub product_id: Uuid,
    pub product: String,
    pub version: String,
    pub file_name: String,
    pub storage_target: Option<String>,
    pub storage_key: String,
    pub sha256_hex: String,
    pub size_bytes: i64,
    pub download_count: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = license_releases)]
pub struct NewLicenseRelease {
    pub id: Uuid,
    pub product_id: Uuid,
    pub product: String,
    pub version: String,
    pub file_name: String,
    pub storage_target: Option<String>,
    pub storage_key: String,
    pub sha256_hex: String,
    pub size_bytes: i64,
    pub download_count: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = license_events)]
pub struct LicenseEvent {
    pub id: Uuid,
    pub license_id: Uuid,
    pub event: String,
    pub actor: Option<String>,
    pub details: Value,
    pub prev_hash: String,
    pub hash: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = license_events)]
pub struct NewLicenseEvent {
    pub id: Uuid,
    pub license_id: Uuid,
    pub event: String,
    pub actor: Option<String>,
    pub details: Value,
    pub prev_hash: String,
    pub hash: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = license_offline_tokens)]
pub struct LicenseOfflineToken {
    pub token_id: Uuid,
    pub license_id: Uuid,
    pub product: String,
    pub expires_at: DateTime<Utc>,
    pub issued_at: DateTime<Utc>,
    pub revoked: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = license_offline_tokens)]
pub struct NewLicenseOfflineToken {
    pub token_id: Uuid,
    pub license_id: Uuid,
    pub product: String,
    pub expires_at: DateTime<Utc>,
    pub issued_at: DateTime<Utc>,
    pub revoked: bool,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = license_activation_history)]
pub struct LicenseActivationHistory {
    pub id: Uuid,
    pub license_id: Uuid,
    pub action: String,
    pub actor: Option<String>,
    pub instance_id: String,
    pub hostname: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = license_activation_history)]
pub struct NewLicenseActivationHistory {
    pub id: Uuid,
    pub license_id: Uuid,
    pub action: String,
    pub actor: Option<String>,
    pub instance_id: String,
    pub hostname: Option<String>,
}

// ---------------------------------------------------------------------------
// SaaS domains / domain verification / cert modes / Cloudflare
// (batch 8 notes §4; struct names as suggested there, `New*`
// structs added per the file convention).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = saas_domain_reservations)]
pub struct SaasDomainReservation {
    pub id: Uuid,
    pub domain: String,
    pub company_id: Uuid,
    pub reserved_by: String,
    pub reserved_until: DateTime<Utc>,
    pub status: String,
    pub site_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = saas_domain_reservations)]
pub struct NewSaasDomainReservation {
    pub id: Uuid,
    pub domain: String,
    pub company_id: Uuid,
    pub reserved_by: String,
    pub reserved_until: DateTime<Utc>,
    pub site_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = saas_domain_aliases)]
pub struct SaasDomainAlias {
    pub id: Uuid,
    pub domain: String,
    pub company_id: Uuid,
    pub target_type: String,
    pub target_value: String,
    pub status: String,
    pub site_id: Option<Uuid>,
    pub verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = saas_domain_aliases)]
pub struct NewSaasDomainAlias {
    pub id: Uuid,
    pub domain: String,
    pub company_id: Uuid,
    pub target_type: String,
    pub target_value: String,
    pub site_id: Option<Uuid>,
    pub verified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = saas_subdomain_allocations)]
pub struct SaasSubdomainAllocation {
    pub id: Uuid,
    pub label: String,
    pub hostname: String,
    pub company_id: Uuid,
    pub site_id: Option<Uuid>,
    pub status: String,
    pub approved_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = saas_subdomain_allocations)]
pub struct NewSaasSubdomainAllocation {
    pub id: Uuid,
    pub label: String,
    pub hostname: String,
    pub company_id: Uuid,
    pub site_id: Option<Uuid>,
    pub approved_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = domain_verification_settings)]
pub struct DomainVerificationSetting {
    pub id: Uuid,
    pub version: i64,
    pub record_type: String,
    pub host_template: String,
    pub value_template: String,
    pub token_length: i32,
    pub token_alphabet: String,
    pub verify_timeout_seconds: i32,
    pub max_attempts: i32,
    pub retry_delay_seconds: i32,
    pub token_ttl_seconds: i64,
    pub rate_limit_window_seconds: i64,
    pub updated_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = domain_verification_settings)]
pub struct NewDomainVerificationSetting {
    pub id: Uuid,
    pub version: i64,
    pub record_type: String,
    pub host_template: String,
    pub value_template: String,
    pub token_length: i32,
    pub token_alphabet: String,
    pub verify_timeout_seconds: i32,
    pub max_attempts: i32,
    pub retry_delay_seconds: i32,
    pub token_ttl_seconds: i64,
    pub rate_limit_window_seconds: i64,
    pub updated_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = domain_verification_tokens)]
pub struct DomainVerificationToken {
    pub id: Uuid,
    pub domain: String,
    pub alias_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub token_hash: String,
    pub record_type: String,
    pub host: String,
    pub expected_value: String,
    pub settings_version: i64,
    pub status: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub verified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = domain_verification_tokens)]
pub struct NewDomainVerificationToken {
    pub id: Uuid,
    pub domain: String,
    pub alias_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub token_hash: String,
    pub record_type: String,
    pub host: String,
    pub expected_value: String,
    pub settings_version: i64,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub verified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = domain_verification_attempts)]
pub struct DomainVerificationAttempt {
    pub id: Uuid,
    pub token_id: Uuid,
    pub domain: String,
    pub alias_id: Option<Uuid>,
    pub attempt_number: i32,
    pub result: String,
    pub record_found: Option<String>,
    pub looked_up: Value,
    pub error: Option<String>,
    pub checked_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = domain_verification_attempts)]
pub struct NewDomainVerificationAttempt {
    pub id: Uuid,
    pub token_id: Uuid,
    pub domain: String,
    pub alias_id: Option<Uuid>,
    pub attempt_number: i32,
    pub result: String,
    pub record_found: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = domain_cert_modes)]
pub struct DomainCertMode {
    pub id: Uuid,
    pub domain: String,
    pub mode: String,
    pub provider: String,
    pub fingerprint_sha256: String,
    pub installed_at: Option<DateTime<Utc>>,
    pub previous_fingerprint: Option<String>,
    pub status: String,
    pub last_error: Option<String>,
    pub updated_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = domain_cert_modes)]
pub struct NewDomainCertMode {
    pub id: Uuid,
    pub domain: String,
    pub mode: String,
    pub provider: String,
    pub installed_at: Option<DateTime<Utc>>,
    pub previous_fingerprint: Option<String>,
    pub status: String,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cloudflare_detections)]
pub struct CloudflareDetection {
    pub id: Uuid,
    pub domain: String,
    pub detected: bool,
    pub methods: Value,
    pub ssl_mode: Option<String>,
    pub end_to_end_encrypted: Option<bool>,
    pub flexible_warning: Option<String>,
    pub checked_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cloudflare_detections)]
pub struct NewCloudflareDetection {
    pub id: Uuid,
    pub domain: String,
    pub ssl_mode: Option<String>,
    pub end_to_end_encrypted: Option<bool>,
    pub flexible_warning: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cloudflare_ip_ranges)]
pub struct CloudflareIpRanges {
    pub key: String,
    pub v4: Value,
    pub v6: Value,
    pub fetched_at: Option<DateTime<Utc>>,
    pub source_v4: String,
    pub source_v6: String,
    pub fetch_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cloudflare_ip_ranges)]
pub struct NewCloudflareIpRanges {
    pub key: String,
    pub fetched_at: Option<DateTime<Utc>>,
    pub fetch_error: Option<String>,
}

// ---------------------------------------------------------------------------
// Whitelabel + presets (batch 9 notes §5; `New*` structs added per
// the file convention, DB-defaulted columns omitted).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = whitelabel_global_settings)]
pub struct WhitelabelGlobalSetting {
    pub id: String,
    pub allow_whitelabel: bool,
    pub updated_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = whitelabel_global_settings)]
pub struct NewWhitelabelGlobalSetting {
    pub id: String,
    pub updated_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = whitelabel_company_settings)]
pub struct WhitelabelCompanySetting {
    pub company_id: String,
    pub mode: String,
    pub updated_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = whitelabel_company_settings)]
pub struct NewWhitelabelCompanySetting {
    pub company_id: String,
    pub updated_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = whitelabel_global_defaults)]
pub struct WhitelabelGlobalDefaults {
    pub id: String,
    pub display_name: String,
    pub logo_storage_target: Option<String>,
    pub logo_storage_key: Option<String>,
    pub favicon_storage_target: Option<String>,
    pub favicon_storage_key: Option<String>,
    pub primary_color: String,
    pub accent_color: String,
    pub background_color: String,
    pub text_color: String,
    pub sender_name: String,
    pub sender_email: String,
    pub login_tagline: String,
    pub login_background_storage_target: Option<String>,
    pub login_background_storage_key: Option<String>,
    pub powered_by_text: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = whitelabel_global_defaults)]
pub struct NewWhitelabelGlobalDefaults {
    pub id: String,
    pub logo_storage_target: Option<String>,
    pub logo_storage_key: Option<String>,
    pub favicon_storage_target: Option<String>,
    pub favicon_storage_key: Option<String>,
    pub login_background_storage_target: Option<String>,
    pub login_background_storage_key: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = whitelabel_profiles)]
pub struct WhitelabelProfile {
    pub id: Uuid,
    pub company_id: String,
    pub tenant_id: Option<String>,
    pub display_name: Option<String>,
    pub logo_storage_target: Option<String>,
    pub logo_storage_key: Option<String>,
    pub favicon_storage_target: Option<String>,
    pub favicon_storage_key: Option<String>,
    pub primary_color: Option<String>,
    pub accent_color: Option<String>,
    pub background_color: Option<String>,
    pub text_color: Option<String>,
    pub custom_domain: Option<String>,
    pub domain_status: String,
    pub hide_powered_by: bool,
    pub sender_name: Option<String>,
    pub sender_email: Option<String>,
    pub login_logo_storage_target: Option<String>,
    pub login_logo_storage_key: Option<String>,
    pub login_background_storage_target: Option<String>,
    pub login_background_storage_key: Option<String>,
    pub login_tagline: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = whitelabel_profiles)]
pub struct NewWhitelabelProfile {
    pub id: Uuid,
    pub company_id: String,
    pub tenant_id: Option<String>,
    pub display_name: Option<String>,
    pub logo_storage_target: Option<String>,
    pub logo_storage_key: Option<String>,
    pub favicon_storage_target: Option<String>,
    pub favicon_storage_key: Option<String>,
    pub primary_color: Option<String>,
    pub accent_color: Option<String>,
    pub background_color: Option<String>,
    pub text_color: Option<String>,
    pub custom_domain: Option<String>,
    pub sender_name: Option<String>,
    pub sender_email: Option<String>,
    pub login_logo_storage_target: Option<String>,
    pub login_logo_storage_key: Option<String>,
    pub login_background_storage_target: Option<String>,
    pub login_background_storage_key: Option<String>,
    pub login_tagline: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = whitelabel_history)]
pub struct WhitelabelHistory {
    pub id: Uuid,
    pub company_id: String,
    pub tenant_id: Option<String>,
    pub actor: String,
    pub action: String,
    pub field: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub allowed: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = whitelabel_history)]
pub struct NewWhitelabelHistory {
    pub id: Uuid,
    pub company_id: String,
    pub tenant_id: Option<String>,
    pub actor: String,
    pub action: String,
    pub field: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = presets)]
pub struct Preset {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub business_type: String,
    pub built_in: bool,
    pub archived: bool,
    pub settings: Value,
    pub steps: Value,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = presets)]
pub struct NewPreset {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub business_type: String,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = preset_modules)]
pub struct PresetModule {
    pub id: Uuid,
    pub preset_id: Uuid,
    pub module_id: String,
    pub required: bool,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = preset_modules)]
pub struct NewPresetModule {
    pub id: Uuid,
    pub preset_id: Uuid,
    pub module_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = preset_wizard_states)]
pub struct PresetWizardState {
    pub id: Uuid,
    pub company_id: Uuid,
    pub actor_id: String,
    pub preset_id: Option<Uuid>,
    pub steps: Value,
    pub current_step: String,
    pub status: String,
    pub skipped_steps: Value,
    pub disabled_optional_modules: Value,
    pub settings: Value,
    pub application_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = preset_wizard_states)]
pub struct NewPresetWizardState {
    pub id: Uuid,
    pub company_id: Uuid,
    pub preset_id: Option<Uuid>,
    pub application_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = preset_applications)]
pub struct PresetApplication {
    pub id: Uuid,
    pub preset_id: Option<Uuid>,
    pub preset_slug: String,
    pub company_id: Uuid,
    pub actor_id: String,
    pub status: String,
    pub outcomes: Value,
    pub settings_applied: Value,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = preset_applications)]
pub struct NewPresetApplication {
    pub id: Uuid,
    pub preset_id: Option<Uuid>,
    pub preset_slug: String,
    pub company_id: Uuid,
    pub completed_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Entitlements (batch 15 B1 notes §4). `EntitlementRow` /
// `NewEntitlementRow` are verbatim from the notes (the given
// `NewEntitlementRow` deliberately carries the DB-defaulted
// `status` / `granted_at`); the other five pairs follow the file
// convention (DB-defaulted columns omitted from `New*`). The notes
// name only a Queryable struct for `entitlement_settings` and
// `entitlement_module_activations`; `New*` structs are added per
// the file convention.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = entitlements)]
pub struct EntitlementRow {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub source_ref: Option<String>,
    pub module_id: String,
    pub status: String,
    pub granted_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub granted_by: Option<String>,
    pub reason: Option<String>,
    pub grace_started_at: Option<DateTime<Utc>>,
    pub grace_ends_at: Option<DateTime<Utc>>,
    pub lapsed_at: Option<DateTime<Utc>>,
    pub lapse_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = entitlements)]
pub struct NewEntitlementRow {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub source_ref: Option<String>,
    pub module_id: String,
    pub status: String,
    pub granted_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub granted_by: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = entitlement_history)]
pub struct EntitlementHistoryRow {
    pub id: Uuid,
    pub tenant_id: String,
    pub entitlement_id: Option<Uuid>,
    pub company_id: String,
    pub module_id: String,
    pub action: String,
    pub actor_id: Option<String>,
    pub detail: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = entitlement_history)]
pub struct NewEntitlementHistoryRow {
    pub id: Uuid,
    pub tenant_id: String,
    pub entitlement_id: Option<Uuid>,
    pub company_id: String,
    pub module_id: String,
    pub action: String,
    pub actor_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = entitlement_denied_attempts)]
pub struct EntitlementDeniedAttemptRow {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub module_id: String,
    pub actor_id: Option<String>,
    pub reason: String,
    pub error_code: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = entitlement_denied_attempts)]
pub struct NewEntitlementDeniedAttemptRow {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub module_id: String,
    pub actor_id: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = entitlement_package_changes)]
pub struct EntitlementPackageChangeRow {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub new_module_ids: Value,
    pub newly_entitled: Value,
    pub kept: Value,
    pub kept_via_other_source: Value,
    pub requires_confirmation: Value,
    pub disabled_module_ids: Value,
    pub status: String,
    pub requested_by: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = entitlement_package_changes)]
pub struct NewEntitlementPackageChangeRow {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub requested_by: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = entitlement_settings)]
pub struct EntitlementSettingsRow {
    pub id: String,
    pub grace_period_days: i64,
    pub read_only_grace_enabled: bool,
    pub default_deny: bool,
    pub updated_by: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = entitlement_settings)]
pub struct NewEntitlementSettingsRow {
    pub id: String,
    pub updated_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = entitlement_module_activations)]
pub struct EntitlementModuleActivationRow {
    pub tenant_id: String,
    pub company_id: String,
    pub module_id: String,
    pub enabled: bool,
    pub enabled_at: DateTime<Utc>,
    pub enabled_by: Option<String>,
    pub source_kind: Option<String>,
    pub source_id: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = entitlement_module_activations)]
pub struct NewEntitlementModuleActivationRow {
    pub tenant_id: String,
    pub company_id: String,
    pub module_id: String,
    pub enabled_by: Option<String>,
    pub source_kind: Option<String>,
    pub source_id: Option<String>,
}

// ---------------------------------------------------------------------------
// SaaS product subdomains (batch 15 B2 notes §2). The notes give
// `ProductSubdomainRow` / `NewProductSubdomainRow` ("same fields
// minus id/timestamps" for `New*`) with `check_for_backend` and the
// `crate::db::schema::` table_name form, both normalised to the
// file convention; the events table has no models in the notes, so
// `SaasProductSubdomainEvent` / `NewSaasProductSubdomainEvent`
// follow the file convention.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = saas_product_subdomains)]
pub struct ProductSubdomainRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub product_id: String,
    pub package_id: Option<String>,
    pub label: String,
    pub host: String,
    pub custom_domain: Option<String>,
    pub site_id: Option<Uuid>,
    pub status: String,
    pub claimed_by: String,
    pub claimed_at: DateTime<Utc>,
    pub verified_at: Option<DateTime<Utc>>,
    pub released_at: Option<DateTime<Utc>>,
    pub grace_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = saas_product_subdomains)]
pub struct NewProductSubdomainRow {
    pub company_id: Uuid,
    pub product_id: String,
    pub package_id: Option<String>,
    pub label: String,
    pub host: String,
    pub custom_domain: Option<String>,
    pub site_id: Option<Uuid>,
    pub status: String,
    pub claimed_by: String,
    pub claimed_at: DateTime<Utc>,
    pub verified_at: Option<DateTime<Utc>>,
    pub released_at: Option<DateTime<Utc>>,
    pub grace_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = saas_product_subdomain_events)]
pub struct SaasProductSubdomainEvent {
    pub id: Uuid,
    pub product_subdomain_id: Uuid,
    pub event: String,
    pub actor: String,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = saas_product_subdomain_events)]
pub struct NewSaasProductSubdomainEvent {
    pub id: Uuid,
    pub product_subdomain_id: Uuid,
    pub event: String,
    pub actor: String,
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Pricing engine (batch 15 B3 notes §6.3). Money structs carry
// `PgNumeric` fields, so per the repo header rule they skip ALL
// serde derives (Queryable: no Serialize; New*: no Deserialize).
// `PricingTier` / `NewPricingTier` already exist for the 000041
// `pricing_tiers` shape and are NOT repeated here.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = pricing_settings)]
pub struct PricingSetting {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub rounding_mode: String,
    pub proration_basis: String,
    pub days_per_month: i64,
    pub approval_threshold_bp: i64,
    pub require_second_approver: bool,
    pub default_currency: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = pricing_settings)]
pub struct NewPricingSetting {
    pub id: Uuid,
    pub tenant_id: Uuid,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = pricing_currencies)]
pub struct PricingCurrency {
    pub code: String,
    pub exponent: i64,
    pub active: bool,
    pub rate_numerator: Option<i64>,
    pub rate_denominator: Option<i64>,
    pub rate_fetched_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = pricing_currencies)]
pub struct NewPricingCurrency {
    pub code: String,
    pub rate_numerator: Option<i64>,
    pub rate_denominator: Option<i64>,
    pub rate_fetched_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pricing_rules)]
pub struct PricingRule {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub kind: String,
    pub percent_bp: i64,
    pub amount_scaled: PgNumeric,
    pub currency: String,
    pub buy_quantity: i64,
    pub get_quantity: i64,
    pub min_quantity: i64,
    pub min_subtotal_scaled: PgNumeric,
    pub product_ids: Value,
    pub evaluation_order: i64,
    pub stackable: bool,
    pub exclusive: bool,
    pub status: String,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pricing_rules)]
pub struct NewPricingRule {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub kind: String,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pricing_coupons)]
pub struct PricingCoupon {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub code: String,
    pub kind: String,
    pub percent_bp: i64,
    pub amount_scaled: PgNumeric,
    pub currency: String,
    pub buy_quantity: i64,
    pub get_quantity: i64,
    pub product_ids: Value,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    pub usage_limit: Option<i64>,
    pub per_customer_limit: Option<i64>,
    pub used_count: i64,
    pub stackable: bool,
    pub exclusive: bool,
    pub active: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pricing_coupons)]
pub struct NewPricingCoupon {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub code: String,
    pub kind: String,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    pub usage_limit: Option<i64>,
    pub per_customer_limit: Option<i64>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = pricing_coupon_redemptions)]
pub struct PricingCouponRedemption {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub coupon_id: Uuid,
    pub customer_id: String,
    pub count: i64,
    pub last_redeemed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = pricing_coupon_redemptions)]
pub struct NewPricingCouponRedemption {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub coupon_id: Uuid,
    pub customer_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pricing_recommendations)]
pub struct PricingRecommendation {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub product_id: String,
    pub kind: String,
    pub suggested_price_scaled: PgNumeric,
    pub currency: String,
    pub lines: Value,
    pub inputs: Value,
    pub status: String,
    pub created_by: String,
    pub confirmed_by: Option<String>,
    pub price_version_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pricing_recommendations)]
pub struct NewPricingRecommendation {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub product_id: String,
    pub kind: String,
    pub suggested_price_scaled: PgNumeric,
    pub created_by: String,
    pub confirmed_by: Option<String>,
    pub price_version_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pricing_cost_lines)]
pub struct PricingCostLine {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub recommendation_id: Uuid,
    pub label: String,
    pub source: String,
    pub cost_scaled: PgNumeric,
    pub currency: String,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pricing_cost_lines)]
pub struct NewPricingCostLine {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub recommendation_id: Uuid,
    pub label: String,
    pub source: String,
    pub cost_scaled: PgNumeric,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pricing_bundles)]
pub struct PricingBundle {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub product_id: String,
    pub discount_kind: String,
    pub discount_percent_bp: i64,
    pub discount_amount_scaled: PgNumeric,
    pub currency: String,
    pub active: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pricing_bundles)]
pub struct NewPricingBundle {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub product_id: String,
    pub discount_kind: String,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pricing_bundle_components)]
pub struct PricingBundleComponent {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub bundle_id: Uuid,
    pub product_id: String,
    pub label: String,
    pub price_scaled: PgNumeric,
    pub currency: String,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pricing_bundle_components)]
pub struct NewPricingBundleComponent {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub bundle_id: Uuid,
    pub product_id: String,
    pub price_scaled: PgNumeric,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pricing_reference_prices)]
pub struct PricingReferencePrice {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub product_id: String,
    pub source: String,
    pub source_url: Option<String>,
    pub price_scaled: PgNumeric,
    pub currency: String,
    pub observed_at: DateTime<Utc>,
    pub note: String,
    pub entered_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pricing_reference_prices)]
pub struct NewPricingReferencePrice {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub product_id: String,
    pub source: String,
    pub source_url: Option<String>,
    pub price_scaled: PgNumeric,
    pub observed_at: DateTime<Utc>,
    pub entered_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pricing_price_versions)]
pub struct PricingPriceVersion {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub product_id: String,
    pub version: i64,
    pub price_scaled: PgNumeric,
    pub currency: String,
    pub previous_price_scaled: PgNumeric,
    pub change_bp: Option<i64>,
    pub above_threshold: bool,
    pub status: String,
    pub requested_by: String,
    pub approved_by: Option<String>,
    pub reason: Option<String>,
    pub recommendation_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pricing_price_versions)]
pub struct NewPricingPriceVersion {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub product_id: String,
    pub version: i64,
    pub price_scaled: PgNumeric,
    pub requested_by: String,
    pub approved_by: Option<String>,
    pub recommendation_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = pricing_price_approvals)]
pub struct PricingPriceApproval {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub price_version_id: Uuid,
    pub approver: String,
    pub decision: String,
    pub reason: Option<String>,
    pub decided_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = pricing_price_approvals)]
pub struct NewPricingPriceApproval {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub price_version_id: Uuid,
    pub approver: String,
    pub decision: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pricing_proration_records)]
pub struct PricingProrationRecord {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub subscription_id: Option<String>,
    pub customer_id: Option<String>,
    pub kind: String,
    pub basis: String,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub change_at: DateTime<Utc>,
    pub period_days: i64,
    pub days_used: i64,
    pub days_remaining: i64,
    pub old_daily_rate_scaled: PgNumeric,
    pub new_daily_rate_scaled: PgNumeric,
    pub unused_credit_scaled: PgNumeric,
    pub new_charge_scaled: PgNumeric,
    pub net_due_scaled: PgNumeric,
    pub currency: String,
    pub lines: Value,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pricing_proration_records)]
pub struct NewPricingProrationRecord {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub subscription_id: Option<String>,
    pub customer_id: Option<String>,
    pub kind: String,
    pub basis: String,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub change_at: DateTime<Utc>,
    pub period_days: i64,
    pub days_used: i64,
    pub days_remaining: i64,
    pub old_daily_rate_scaled: PgNumeric,
    pub new_daily_rate_scaled: PgNumeric,
    pub unused_credit_scaled: PgNumeric,
    pub new_charge_scaled: PgNumeric,
    pub net_due_scaled: PgNumeric,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pricing_credit_notes)]
pub struct PricingCreditNote {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub number: String,
    pub invoice_id: String,
    pub amount_scaled: PgNumeric,
    pub currency: String,
    pub lines: Value,
    pub reason: String,
    pub status: String,
    pub reconciled: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pricing_credit_notes)]
pub struct NewPricingCreditNote {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub number: String,
    pub invoice_id: String,
    pub amount_scaled: PgNumeric,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pricing_reconciliation_reports)]
pub struct PricingReconciliationReport {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub invoice_id: Option<String>,
    pub matched: bool,
    pub currency: String,
    pub components: Value,
    pub total_discrepancy_scaled: PgNumeric,
    pub checked_by: String,
    pub checked_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pricing_reconciliation_reports)]
pub struct NewPricingReconciliationReport {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub invoice_id: Option<String>,
    pub matched: bool,
    pub checked_by: String,
}

// ---------------------------------------------------------------------------
// Storage product (storage product notes §4). The notes mirror the
// module structs field-for-field, so `MeteringReport`,
// `ByoTermsAcceptance` and `ByoHealthCheck` carry no
// created_at/updated_at even though the tables have them.
// `ByoConnection.credentials_encrypted` is marked
// `#[serde(skip_serializing)]` per the notes.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_plans)]
pub struct StoragePlan {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub quota_bytes: i64,
    pub max_files: i64,
    pub included_provider_kinds: Vec<String>,
    pub product_id: Option<String>,
    pub billing_cycle: Option<String>,
    pub is_public: bool,
    pub sort_order: i64,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_plans)]
pub struct NewStoragePlan {
    pub id: Uuid,
    pub name: String,
    pub product_id: Option<String>,
    pub billing_cycle: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_plan_assignments)]
pub struct PlanAssignment {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub company_id: Option<String>,
    pub user_id: String,
    pub status: String,
    pub assigned_by: String,
    pub assigned_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_plan_assignments)]
pub struct NewPlanAssignment {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub company_id: Option<String>,
    pub user_id: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_usage_metering)]
pub struct MeteringReport {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub user_id: String,
    pub plan_id: Option<Uuid>,
    pub period: String,
    pub bytes_stored: i64,
    pub files_stored: i64,
    pub reported_by: String,
    pub reported_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_usage_metering)]
pub struct NewMeteringReport {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub user_id: String,
    pub plan_id: Option<Uuid>,
    pub period: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_byo_connections)]
pub struct ByoConnection {
    pub id: Uuid,
    pub name: String,
    pub provider_kind: String,
    pub s3_compatible: bool,
    pub endpoint: String,
    pub region: String,
    pub container: String,
    pub path: String,
    pub sftp_host: String,
    pub sftp_port: i32,
    pub sftp_username: String,
    pub sftp_auth_kind: String,
    pub sftp_keyfile: String,
    pub allowed_hosts: Vec<String>,
    #[serde(skip_serializing)]
    pub credentials_encrypted: Option<String>,
    pub key_fingerprint: Option<String>,
    pub path_prefix: String,
    pub owner_user: String,
    pub company_id: Option<String>,
    pub status: String,
    pub status_reason: Option<String>,
    pub allow_read: bool,
    pub allow_write: bool,
    pub allow_delete: bool,
    pub max_bytes: Option<i64>,
    pub max_files: Option<i64>,
    pub used_bytes: i64,
    pub used_files: i64,
    pub encryption_required: bool,
    pub allowed_extensions: Vec<String>,
    pub allowed_mime_types: Vec<String>,
    pub terms_version: Option<String>,
    pub terms_accepted_at: Option<DateTime<Utc>>,
    pub last_validated_at: Option<DateTime<Utc>>,
    pub last_validation_ok: Option<bool>,
    pub last_validation_error: Option<String>,
    pub last_health_at: Option<DateTime<Utc>>,
    pub last_health_ok: Option<bool>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_byo_connections)]
pub struct NewByoConnection {
    pub id: Uuid,
    pub name: String,
    pub provider_kind: String,
    pub credentials_encrypted: Option<String>,
    pub key_fingerprint: Option<String>,
    pub owner_user: String,
    pub company_id: Option<String>,
    pub status_reason: Option<String>,
    pub max_bytes: Option<i64>,
    pub max_files: Option<i64>,
    pub terms_version: Option<String>,
    pub terms_accepted_at: Option<DateTime<Utc>>,
    pub last_validated_at: Option<DateTime<Utc>>,
    pub last_validation_ok: Option<bool>,
    pub last_validation_error: Option<String>,
    pub last_health_at: Option<DateTime<Utc>>,
    pub last_health_ok: Option<bool>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_byo_terms)]
pub struct ByoTerms {
    pub id: Uuid,
    pub version: String,
    pub title: String,
    pub body: String,
    pub is_current: bool,
    pub reacceptance_deadline: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_byo_terms)]
pub struct NewByoTerms {
    pub id: Uuid,
    pub version: String,
    pub title: String,
    pub reacceptance_deadline: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_byo_terms_acceptances)]
pub struct ByoTermsAcceptance {
    pub id: Uuid,
    pub terms_id: Uuid,
    pub terms_version: String,
    pub user_id: String,
    pub company_id: Option<String>,
    pub connection_id: Option<Uuid>,
    pub accepted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_byo_terms_acceptances)]
pub struct NewByoTermsAcceptance {
    pub id: Uuid,
    pub terms_id: Uuid,
    pub terms_version: String,
    pub user_id: String,
    pub company_id: Option<String>,
    pub connection_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_byo_objects)]
pub struct ByoObject {
    pub connection_id: Uuid,
    pub key: String,
    pub size_bytes: i64,
    pub mime: String,
    pub owner_user: String,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_byo_objects)]
pub struct NewByoObject {
    pub connection_id: Uuid,
    pub key: String,
    pub owner_user: String,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_byo_health_checks)]
pub struct ByoHealthCheck {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub ok: bool,
    pub latency_ms: i64,
    pub detail: String,
    pub checked_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_byo_health_checks)]
pub struct NewByoHealthCheck {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub ok: bool,
}

// ---------------------------------------------------------------------------
// Card validation + reference-id settings (batch 18 notes §3b/§3e).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = card_custom_rules)]
pub struct CardCustomRule {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub brand_name: String,
    pub iin_prefixes: Value,
    pub iin_ranges: Value,
    pub allowed_lengths: Value,
    pub cvv_length: i16,
    pub luhn_required: bool,
    pub enabled: bool,
    pub overrides_standard_brand: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = card_custom_rules)]
pub struct NewCardCustomRule {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub brand_name: String,
    pub overrides_standard_brand: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = card_validation_daily_counts)]
pub struct CardValidationDailyCount {
    pub tenant_id: String,
    pub date: NaiveDate,
    pub matched_rule: String,
    pub result: String,
    pub count: i64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = card_validation_daily_counts)]
pub struct NewCardValidationDailyCount {
    pub tenant_id: String,
    pub date: NaiveDate,
    pub matched_rule: String,
    pub result: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = reference_id_settings)]
pub struct ReferenceIdSetting {
    pub tenant_id: Uuid,
    pub default_prefix: String,
    pub product_prefixes: Value,
    pub max_total_length: i32,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = reference_id_settings)]
pub struct NewReferenceIdSetting {
    pub tenant_id: Uuid,
}

// ---------------------------------------------------------------------------
// Tenants (batch 17; models derived from the `tenants.rs` header
// snippets — no model structs were given in the notes).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = tenants)]
pub struct Tenant {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub status: String,
    pub owner_user_id: Uuid,
    pub package_id: Option<Uuid>,
    pub preset_id: Option<Uuid>,
    pub plan_id: Option<String>,
    pub subdomain: Option<String>,
    pub subdomain_pending: bool,
    pub schema_name: String,
    pub suspended_at: Option<DateTime<Utc>>,
    pub suspended_reason: Option<String>,
    pub destroyed_at: Option<DateTime<Utc>>,
    pub export_reference: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = tenants)]
pub struct NewTenant {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub owner_user_id: Uuid,
    pub package_id: Option<Uuid>,
    pub preset_id: Option<Uuid>,
    pub plan_id: Option<String>,
    pub subdomain: Option<String>,
    pub schema_name: String,
    pub suspended_at: Option<DateTime<Utc>>,
    pub suspended_reason: Option<String>,
    pub destroyed_at: Option<DateTime<Utc>>,
    pub export_reference: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = tenant_provisioning_steps)]
pub struct TenantProvisioningStep {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub step: String,
    pub status: String,
    pub attempt: i32,
    pub error: Option<String>,
    pub detail: Value,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = tenant_provisioning_steps)]
pub struct NewTenantProvisioningStep {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub step: String,
    pub error: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = tenant_module_grants)]
pub struct TenantModuleGrant {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub module_id: String,
    pub granted_via: String,
    pub granted_via_id: Option<Uuid>,
    pub granted_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub granted_by: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoke_reason: Option<String>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = tenant_module_grants)]
pub struct NewTenantModuleGrant {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub module_id: String,
    pub granted_via: String,
    pub granted_via_id: Option<Uuid>,
    pub expires_at: Option<DateTime<Utc>>,
    pub granted_by: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoke_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Business Intelligence (migration 000042_bi).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = bi_reports)]
pub struct BiReport {
    pub id: Uuid,
    pub company_id: String,
    pub owner_id: String,
    pub name: String,
    pub description: Option<String>,
    pub source: String,
    pub fields: Value,
    pub filters: Value,
    pub group_by: Option<String>,
    pub chart_type: String,
    pub visibility: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = bi_reports)]
pub struct NewBiReport {
    pub id: Uuid,
    pub company_id: String,
    pub owner_id: String,
    pub name: String,
    pub description: Option<String>,
    pub source: String,
    pub group_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = bi_report_schedules)]
pub struct BiReportSchedule {
    pub id: Uuid,
    pub report_id: Uuid,
    pub company_id: String,
    pub schedule: String,
    pub recipients: Value,
    pub channel: String,
    pub scheduler_status: String,
    pub scheduler_job_id: Option<String>,
    pub last_delivered_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = bi_report_schedules)]
pub struct NewBiReportSchedule {
    pub id: Uuid,
    pub report_id: Uuid,
    pub company_id: String,
    pub schedule: String,
    pub scheduler_job_id: Option<String>,
    pub last_delivered_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = bi_kpi_dashboards)]
pub struct BiKpiDashboard {
    pub id: Uuid,
    pub company_id: String,
    pub owner_id: String,
    pub name: String,
    pub description: Option<String>,
    pub kpis: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = bi_kpi_dashboards)]
pub struct NewBiKpiDashboard {
    pub id: Uuid,
    pub company_id: String,
    pub owner_id: String,
    pub name: String,
    pub description: Option<String>,
}

// ---------------------------------------------------------------------------
// OKRs (migration 000043_okrs).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = okr_cycles)]
pub struct OkrCycle {
    pub id: Uuid,
    pub company_id: String,
    pub name: String,
    pub description: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = okr_cycles)]
pub struct NewOkrCycle {
    pub id: Uuid,
    pub company_id: String,
    pub name: String,
    pub description: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = okr_objectives)]
pub struct OkrObjective {
    pub id: Uuid,
    pub cycle_id: Uuid,
    pub company_id: String,
    pub parent_objective_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub level: String,
    pub owner_id: String,
    pub final_score: Option<PgNumeric>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = okr_objectives)]
pub struct NewOkrObjective {
    pub id: Uuid,
    pub cycle_id: Uuid,
    pub company_id: String,
    pub parent_objective_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub level: String,
    pub owner_id: String,
    pub final_score: Option<PgNumeric>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = okr_key_results)]
pub struct OkrKeyResult {
    pub id: Uuid,
    pub objective_id: Uuid,
    pub title: String,
    pub unit: String,
    pub target_value: PgNumeric,
    pub start_value: PgNumeric,
    pub current_value: PgNumeric,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = okr_key_results)]
pub struct NewOkrKeyResult {
    pub id: Uuid,
    pub objective_id: Uuid,
    pub title: String,
    pub unit: String,
    pub target_value: PgNumeric,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = okr_checkins)]
pub struct OkrCheckin {
    pub id: Uuid,
    pub key_result_id: Uuid,
    pub author_id: String,
    pub note: String,
    pub confidence: String,
    pub kind: String,
    pub reason: Option<String>,
    pub value_snapshot: Option<PgNumeric>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = okr_checkins)]
pub struct NewOkrCheckin {
    pub id: Uuid,
    pub key_result_id: Uuid,
    pub author_id: String,
    pub note: String,
    pub confidence: String,
    pub reason: Option<String>,
    pub value_snapshot: Option<PgNumeric>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = okr_links)]
pub struct OkrLink {
    pub id: Uuid,
    pub key_result_id: Uuid,
    pub link_type: String,
    pub target_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = okr_links)]
pub struct NewOkrLink {
    pub id: Uuid,
    pub key_result_id: Uuid,
    pub link_type: String,
    pub target_id: Uuid,
}

// ---------------------------------------------------------------------------
// Affiliates (migration 000044_affiliates).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = affiliates)]
pub struct Affiliate {
    pub id: Uuid,
    pub user_id: String,
    pub display_name: String,
    pub email: String,
    pub status: String,
    pub commission_type: String,
    pub commission_value: PgNumeric,
    pub currency: String,
    pub attribution_window_days: i32,
    pub company_id: Option<String>,
    pub payout_method: Option<String>,
    pub payout_reference: Option<String>,
    pub reviewed_by: Option<String>,
    pub review_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = affiliates)]
pub struct NewAffiliate {
    pub id: Uuid,
    pub user_id: String,
    pub display_name: String,
    pub email: String,
    pub company_id: Option<String>,
    pub payout_method: Option<String>,
    pub payout_reference: Option<String>,
    pub reviewed_by: Option<String>,
    pub review_reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = affiliate_commission_rules)]
pub struct AffiliateCommissionRule {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub product_id: String,
    pub commission_type: String,
    pub commission_value: PgNumeric,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = affiliate_commission_rules)]
pub struct NewAffiliateCommissionRule {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub product_id: String,
    pub commission_type: String,
    pub commission_value: PgNumeric,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = affiliate_links)]
pub struct AffiliateLink {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub code: String,
    pub campaign: Option<String>,
    pub product_id: Option<String>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = affiliate_links)]
pub struct NewAffiliateLink {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub code: String,
    pub campaign: Option<String>,
    pub product_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = affiliate_clicks)]
pub struct AffiliateClick {
    pub id: Uuid,
    pub link_id: Uuid,
    pub affiliate_id: Uuid,
    pub ip_hash: String,
    pub user_agent: Option<String>,
    pub referrer: Option<String>,
    pub flagged: bool,
    pub flag_reasons: Vec<String>,
    pub converted: bool,
    pub clicked_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = affiliate_clicks)]
pub struct NewAffiliateClick {
    pub id: Uuid,
    pub link_id: Uuid,
    pub affiliate_id: Uuid,
    pub ip_hash: String,
    pub user_agent: Option<String>,
    pub referrer: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = affiliate_conversions)]
pub struct AffiliateConversion {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub link_id: Option<Uuid>,
    pub click_id: Option<Uuid>,
    pub customer_user_id: String,
    pub kind: String,
    pub order_id: Option<String>,
    pub order_total: PgNumeric,
    pub currency: String,
    pub product_id: Option<String>,
    pub flagged: bool,
    pub flag_reasons: Vec<String>,
    pub attributed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = affiliate_conversions)]
pub struct NewAffiliateConversion {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub link_id: Option<Uuid>,
    pub click_id: Option<Uuid>,
    pub customer_user_id: String,
    pub kind: String,
    pub order_id: Option<String>,
    pub product_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = affiliate_commissions)]
pub struct AffiliateCommission {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub conversion_id: Uuid,
    pub order_id: Option<String>,
    pub product_id: Option<String>,
    pub commission_type: String,
    pub rate_or_amount: PgNumeric,
    pub amount: PgNumeric,
    pub currency: String,
    pub status: String,
    pub payout_id: Option<Uuid>,
    pub flagged: bool,
    pub fraud_flags: Vec<String>,
    pub reviewed_by: Option<String>,
    pub review_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = affiliate_commissions)]
pub struct NewAffiliateCommission {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub conversion_id: Uuid,
    pub order_id: Option<String>,
    pub product_id: Option<String>,
    pub commission_type: String,
    pub rate_or_amount: PgNumeric,
    pub amount: PgNumeric,
    pub payout_id: Option<Uuid>,
    pub reviewed_by: Option<String>,
    pub review_reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = affiliate_payouts)]
pub struct AffiliatePayout {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub commission_ids: Vec<Uuid>,
    pub total_amount: PgNumeric,
    pub currency: String,
    pub status: String,
    pub method: Option<String>,
    pub reference: Option<String>,
    pub requested_by: String,
    pub reviewed_by: Option<String>,
    pub review_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = affiliate_payouts)]
pub struct NewAffiliatePayout {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub total_amount: PgNumeric,
    pub method: Option<String>,
    pub reference: Option<String>,
    pub requested_by: String,
    pub reviewed_by: Option<String>,
    pub review_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Loyalty (migration 000045_loyalty).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = loyalty_programs)]
pub struct LoyaltyProgram {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub points_name: String,
    pub currency: String,
    pub discount_per_point: PgNumeric,
    pub active: bool,
    pub expiry_days: Option<i32>,
    pub expiry_mode: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = loyalty_programs)]
pub struct NewLoyaltyProgram {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub expiry_days: Option<i32>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = loyalty_earn_rules)]
pub struct LoyaltyEarnRule {
    pub id: Uuid,
    pub program_id: Uuid,
    pub kind: String,
    pub points_per_unit: PgNumeric,
    pub fixed_points: Option<i64>,
    pub cap_per_day: Option<i64>,
    pub cap_total: Option<i64>,
    pub min_order_amount: Option<PgNumeric>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = loyalty_earn_rules)]
pub struct NewLoyaltyEarnRule {
    pub id: Uuid,
    pub program_id: Uuid,
    pub kind: String,
    pub fixed_points: Option<i64>,
    pub cap_per_day: Option<i64>,
    pub cap_total: Option<i64>,
    pub min_order_amount: Option<PgNumeric>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = loyalty_tiers)]
pub struct LoyaltyTier {
    pub id: Uuid,
    pub program_id: Uuid,
    pub name: String,
    pub min_lifetime_points: i64,
    pub min_lifetime_spend: PgNumeric,
    pub benefits: Vec<String>,
    pub discount_percent: PgNumeric,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = loyalty_tiers)]
pub struct NewLoyaltyTier {
    pub id: Uuid,
    pub program_id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = loyalty_ledger)]
pub struct LoyaltyLedger {
    pub id: Uuid,
    pub program_id: Uuid,
    pub customer_id: String,
    pub kind: String,
    pub points: i64,
    pub balance_after: i64,
    pub reason: String,
    pub actor: String,
    pub earn_kind: Option<String>,
    pub reference_id: Option<String>,
    pub reference_amount: Option<PgNumeric>,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = loyalty_ledger)]
pub struct NewLoyaltyLedger {
    pub id: Uuid,
    pub program_id: Uuid,
    pub customer_id: String,
    pub kind: String,
    pub points: i64,
    pub balance_after: i64,
    pub reason: String,
    pub actor: String,
    pub earn_kind: Option<String>,
    pub reference_id: Option<String>,
    pub reference_amount: Option<PgNumeric>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = loyalty_redemptions)]
pub struct LoyaltyRedemption {
    pub id: Uuid,
    pub program_id: Uuid,
    pub customer_id: String,
    pub points_spent: i64,
    pub kind: String,
    pub discount_amount: PgNumeric,
    pub currency: String,
    pub product_id: Option<String>,
    pub order_id: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = loyalty_redemptions)]
pub struct NewLoyaltyRedemption {
    pub id: Uuid,
    pub program_id: Uuid,
    pub customer_id: String,
    pub points_spent: i64,
    pub kind: String,
    pub product_id: Option<String>,
    pub order_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Client portals (migration 000046_portals).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = portals)]
pub struct Portal {
    pub id: Uuid,
    pub company_id: String,
    pub name: String,
    pub slug: String,
    pub custom_domain: Option<String>,
    pub domain_status: String,
    pub logo_storage_target: Option<String>,
    pub logo_storage_key: Option<String>,
    pub primary_color: String,
    pub secondary_color: String,
    pub accent_color: String,
    pub background_color: String,
    pub text_color: String,
    pub sections: Vec<String>,
    pub status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = portals)]
pub struct NewPortal {
    pub id: Uuid,
    pub company_id: String,
    pub name: String,
    pub slug: String,
    pub custom_domain: Option<String>,
    pub logo_storage_target: Option<String>,
    pub logo_storage_key: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = portal_menus)]
pub struct PortalMenu {
    pub id: Uuid,
    pub portal_id: Uuid,
    pub label: String,
    pub target_kind: String,
    pub target: String,
    pub position: i64,
    pub visible: bool,
    pub roles: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = portal_menus)]
pub struct NewPortalMenu {
    pub id: Uuid,
    pub portal_id: Uuid,
    pub label: String,
    pub target: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = portal_views)]
pub struct PortalView {
    pub id: Uuid,
    pub portal_id: Uuid,
    pub slug: String,
    pub viewer_user_id: Option<String>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub viewed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = portal_views)]
pub struct NewPortalView {
    pub id: Uuid,
    pub portal_id: Uuid,
    pub slug: String,
    pub viewer_user_id: Option<String>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
}

// ---------------------------------------------------------------------------
// Forms (migration 000047_forms).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = forms)]
pub struct Form {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub slug: String,
    pub status: String,
    pub company_id: Option<String>,
    pub owner_id: String,
    pub submit_button_label: String,
    pub success_message: String,
    pub honeypot_field: String,
    pub mapping: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = forms)]
pub struct NewForm {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub slug: String,
    pub company_id: Option<String>,
    pub owner_id: String,
    pub mapping: Option<Value>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = form_fields)]
pub struct FormField {
    pub id: Uuid,
    pub form_id: Uuid,
    pub key: String,
    pub label: String,
    pub field_type: String,
    pub position: i64,
    pub required: bool,
    pub placeholder: Option<String>,
    pub help_text: Option<String>,
    pub options: Vec<String>,
    pub min_length: Option<i64>,
    pub max_length: Option<i64>,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
    pub pattern: Option<String>,
    pub show_when: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = form_fields)]
pub struct NewFormField {
    pub id: Uuid,
    pub form_id: Uuid,
    pub key: String,
    pub label: String,
    pub field_type: String,
    pub placeholder: Option<String>,
    pub help_text: Option<String>,
    pub min_length: Option<i64>,
    pub max_length: Option<i64>,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
    pub pattern: Option<String>,
    pub show_when: Option<Value>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = form_submissions)]
pub struct FormSubmission {
    pub id: Uuid,
    pub form_id: Uuid,
    pub status: String,
    pub mapping_status: String,
    pub mapping_error: Option<String>,
    pub mapped_target_id: Option<String>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub submitted_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = form_submissions)]
pub struct NewFormSubmission {
    pub id: Uuid,
    pub form_id: Uuid,
    pub mapping_error: Option<String>,
    pub mapped_target_id: Option<String>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub submitted_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = form_submission_values)]
pub struct FormSubmissionValue {
    pub id: Uuid,
    pub submission_id: Uuid,
    pub field_id: Uuid,
    pub field_key: String,
    pub value: Value,
    pub file_storage_key: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = form_submission_values)]
pub struct NewFormSubmissionValue {
    pub id: Uuid,
    pub submission_id: Uuid,
    pub field_id: Uuid,
    pub field_key: String,
    pub file_storage_key: Option<String>,
}

// ---------------------------------------------------------------------------
// Contracts (migration 000048_contracts).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = contract_templates)]
pub struct ContractTemplate {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub body: String,
    pub placeholders: Vec<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = contract_templates)]
pub struct NewContractTemplate {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub body: String,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = contracts)]
pub struct Contract {
    pub id: Uuid,
    pub template_id: Option<Uuid>,
    pub title: String,
    pub company_party: String,
    pub counterparty_name: String,
    pub counterparty_email: String,
    pub terms: String,
    pub value: PgNumeric,
    pub currency: String,
    pub signing_order: String,
    pub status: String,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub signed_at: Option<DateTime<Utc>>,
    pub content_hash: Option<String>,
    pub void_reason: Option<String>,
    pub terminate_reason: Option<String>,
    pub renewed_from_id: Option<Uuid>,
    pub alert_days: Vec<i32>,
    pub expiry_alert_status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = contracts)]
pub struct NewContract {
    pub id: Uuid,
    pub template_id: Option<Uuid>,
    pub title: String,
    pub company_party: String,
    pub counterparty_name: String,
    pub counterparty_email: String,
    pub terms: String,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub signed_at: Option<DateTime<Utc>>,
    pub content_hash: Option<String>,
    pub void_reason: Option<String>,
    pub terminate_reason: Option<String>,
    pub renewed_from_id: Option<Uuid>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = contract_signers)]
pub struct ContractSigner {
    pub id: Uuid,
    pub contract_id: Uuid,
    pub name: String,
    pub email: String,
    pub order_index: i32,
    pub status: String,
    pub token_hash: String,
    pub token_expires_at: DateTime<Utc>,
    pub signed_at: Option<DateTime<Utc>>,
    pub declined_reason: Option<String>,
    pub viewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = contract_signers)]
pub struct NewContractSigner {
    pub id: Uuid,
    pub contract_id: Uuid,
    pub name: String,
    pub email: String,
    pub token_hash: String,
    pub token_expires_at: DateTime<Utc>,
    pub signed_at: Option<DateTime<Utc>>,
    pub declined_reason: Option<String>,
    pub viewed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = contract_audit_trail)]
pub struct ContractAuditTrail {
    pub id: Uuid,
    pub contract_id: Uuid,
    pub signer_id: Option<Uuid>,
    pub actor: String,
    pub action: String,
    pub ip_hash: String,
    pub user_agent_hash: String,
    pub details: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = contract_audit_trail)]
pub struct NewContractAuditTrail {
    pub id: Uuid,
    pub contract_id: Uuid,
    pub signer_id: Option<Uuid>,
    pub actor: String,
    pub action: String,
    pub ip_hash: String,
    pub user_agent_hash: String,
    pub details: Option<String>,
}

// ---------------------------------------------------------------------------
// Procurement (migration 000049_procurement).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = vendors)]
pub struct Vendor {
    pub id: Uuid,
    pub name: String,
    pub contact_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub address: Option<String>,
    pub categories: Vec<String>,
    pub tags: Vec<String>,
    pub status: String,
    pub payment_reference: Option<String>,
    pub notes: Option<String>,
    pub rating: Option<i16>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = vendors)]
pub struct NewVendor {
    pub id: Uuid,
    pub name: String,
    pub contact_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub address: Option<String>,
    pub payment_reference: Option<String>,
    pub notes: Option<String>,
    pub rating: Option<i16>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = purchase_orders)]
pub struct PurchaseOrder {
    pub id: Uuid,
    pub po_number: String,
    pub vendor_id: Uuid,
    pub title: String,
    pub category: Option<String>,
    pub currency: String,
    pub status: String,
    pub subtotal: PgNumeric,
    pub tax_total: PgNumeric,
    pub total: PgNumeric,
    pub promised_date: Option<NaiveDate>,
    pub ordered_at: Option<DateTime<Utc>>,
    pub received_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
    pub requested_by: String,
    pub approved_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = purchase_orders)]
pub struct NewPurchaseOrder {
    pub id: Uuid,
    pub po_number: String,
    pub vendor_id: Uuid,
    pub title: String,
    pub category: Option<String>,
    pub promised_date: Option<NaiveDate>,
    pub ordered_at: Option<DateTime<Utc>>,
    pub received_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
    pub requested_by: String,
    pub approved_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = purchase_order_lines)]
pub struct PurchaseOrderLine {
    pub id: Uuid,
    pub purchase_order_id: Uuid,
    pub description: String,
    pub quantity: PgNumeric,
    pub unit_price: PgNumeric,
    pub tax_rate: PgNumeric,
    pub line_total: PgNumeric,
    pub received_quantity: PgNumeric,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = purchase_order_lines)]
pub struct NewPurchaseOrderLine {
    pub id: Uuid,
    pub purchase_order_id: Uuid,
    pub description: String,
    pub quantity: PgNumeric,
    pub unit_price: PgNumeric,
    pub line_total: PgNumeric,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = purchase_order_approvals)]
pub struct PurchaseOrderApproval {
    pub id: Uuid,
    pub purchase_order_id: Uuid,
    pub approver_id: String,
    pub approver_role: String,
    pub decision: String,
    pub reason: Option<String>,
    pub decided_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = purchase_order_approvals)]
pub struct NewPurchaseOrderApproval {
    pub id: Uuid,
    pub purchase_order_id: Uuid,
    pub approver_id: String,
    pub approver_role: String,
    pub decision: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = goods_receipts)]
pub struct GoodsReceipt {
    pub id: Uuid,
    pub purchase_order_id: Uuid,
    pub line_id: Uuid,
    pub quantity_received: PgNumeric,
    pub quality_note: Option<String>,
    pub received_by: String,
    pub received_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = goods_receipts)]
pub struct NewGoodsReceipt {
    pub id: Uuid,
    pub purchase_order_id: Uuid,
    pub line_id: Uuid,
    pub quantity_received: PgNumeric,
    pub quality_note: Option<String>,
    pub received_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = procurement_budgets)]
pub struct ProcurementBudget {
    pub id: Uuid,
    pub category: Option<String>,
    pub vendor_id: Option<Uuid>,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub budget_amount: PgNumeric,
    pub currency: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = procurement_budgets)]
pub struct NewProcurementBudget {
    pub id: Uuid,
    pub category: Option<String>,
    pub vendor_id: Option<Uuid>,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub budget_amount: PgNumeric,
    pub created_by: String,
}

// ---------------------------------------------------------------------------
// Assets (migration 000050_assets).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = assets)]
pub struct Asset {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub asset_type: String,
    pub serial_number: Option<String>,
    pub purchase_date: Option<NaiveDate>,
    pub purchase_price: PgNumeric,
    pub currency: String,
    pub vendor: Option<String>,
    pub location: Option<String>,
    pub status: String,
    pub warranty_end: Option<NaiveDate>,
    pub license_expires_at: Option<NaiveDate>,
    pub license_seats: Option<i64>,
    pub depreciation_method: Option<String>,
    pub useful_life_years: Option<i64>,
    pub salvage_value: PgNumeric,
    pub depreciation_status: String,
    pub depreciation_schedule: Option<Value>,
    pub depreciation_error: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = assets)]
pub struct NewAsset {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub serial_number: Option<String>,
    pub purchase_date: Option<NaiveDate>,
    pub vendor: Option<String>,
    pub location: Option<String>,
    pub warranty_end: Option<NaiveDate>,
    pub license_expires_at: Option<NaiveDate>,
    pub license_seats: Option<i64>,
    pub depreciation_method: Option<String>,
    pub useful_life_years: Option<i64>,
    pub depreciation_schedule: Option<Value>,
    pub depreciation_error: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = asset_assignments)]
pub struct AssetAssignment {
    pub id: Uuid,
    pub asset_id: Uuid,
    pub user_id: String,
    pub assigned_by: String,
    pub assigned_at: DateTime<Utc>,
    pub returned_at: Option<DateTime<Utc>>,
    pub return_reason: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = asset_assignments)]
pub struct NewAssetAssignment {
    pub id: Uuid,
    pub asset_id: Uuid,
    pub user_id: String,
    pub assigned_by: String,
    pub returned_at: Option<DateTime<Utc>>,
    pub return_reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = asset_stock)]
pub struct AssetStock {
    pub id: Uuid,
    pub asset_id: Uuid,
    pub quantity_on_hand: i64,
    pub reorder_threshold: i64,
    pub location: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = asset_stock)]
pub struct NewAssetStock {
    pub id: Uuid,
    pub asset_id: Uuid,
    pub location: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = asset_stock_adjustments)]
pub struct AssetStockAdjustment {
    pub id: Uuid,
    pub stock_id: Uuid,
    pub asset_id: Uuid,
    pub delta: i64,
    pub quantity_after: i64,
    pub reason: String,
    pub adjusted_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = asset_stock_adjustments)]
pub struct NewAssetStockAdjustment {
    pub id: Uuid,
    pub stock_id: Uuid,
    pub asset_id: Uuid,
    pub delta: i64,
    pub quantity_after: i64,
    pub reason: String,
    pub adjusted_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = asset_stock_takes)]
pub struct AssetStockTake {
    pub id: Uuid,
    pub title: String,
    pub notes: Option<String>,
    pub status: String,
    pub started_by: String,
    pub started_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub closed_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = asset_stock_takes)]
pub struct NewAssetStockTake {
    pub id: Uuid,
    pub title: String,
    pub notes: Option<String>,
    pub started_by: String,
    pub closed_at: Option<DateTime<Utc>>,
    pub closed_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = asset_stock_take_lines)]
pub struct AssetStockTakeLine {
    pub id: Uuid,
    pub stock_take_id: Uuid,
    pub asset_id: Uuid,
    pub stock_id: Uuid,
    pub expected_quantity: i64,
    pub counted_quantity: Option<i64>,
    pub counted_by: Option<String>,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = asset_stock_take_lines)]
pub struct NewAssetStockTakeLine {
    pub id: Uuid,
    pub stock_take_id: Uuid,
    pub asset_id: Uuid,
    pub stock_id: Uuid,
    pub expected_quantity: i64,
    pub counted_quantity: Option<i64>,
    pub counted_by: Option<String>,
    pub note: Option<String>,
}

// ---------------------------------------------------------------------------
// Recruitment (migration 000051_recruitment).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = jobs)]
pub struct Job {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub department_id: Option<String>,
    pub location: Option<String>,
    pub job_type: String,
    pub salary_min: Option<PgNumeric>,
    pub salary_max: Option<PgNumeric>,
    pub currency: String,
    pub status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = jobs)]
pub struct NewJob {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub department_id: Option<String>,
    pub location: Option<String>,
    pub salary_min: Option<PgNumeric>,
    pub salary_max: Option<PgNumeric>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = applicants)]
pub struct Applicant {
    pub id: Uuid,
    pub job_id: Uuid,
    pub name: String,
    pub email: String,
    pub phone: Option<String>,
    pub source: Option<String>,
    pub resume_storage_target: Option<String>,
    pub resume_storage_key: Option<String>,
    pub consent_contact: bool,
    pub consent_at: Option<DateTime<Utc>>,
    pub stage: String,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = applicants)]
pub struct NewApplicant {
    pub id: Uuid,
    pub job_id: Uuid,
    pub name: String,
    pub email: String,
    pub phone: Option<String>,
    pub source: Option<String>,
    pub resume_storage_target: Option<String>,
    pub resume_storage_key: Option<String>,
    pub consent_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = applicant_stage_history)]
pub struct ApplicantStageHistory {
    pub id: Uuid,
    pub applicant_id: Uuid,
    pub job_id: Uuid,
    pub from_stage: Option<String>,
    pub to_stage: String,
    pub moved_by: String,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = applicant_stage_history)]
pub struct NewApplicantStageHistory {
    pub id: Uuid,
    pub applicant_id: Uuid,
    pub job_id: Uuid,
    pub from_stage: Option<String>,
    pub to_stage: String,
    pub moved_by: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = interviews)]
pub struct Interview {
    pub id: Uuid,
    pub applicant_id: Uuid,
    pub job_id: Uuid,
    pub scheduled_at: DateTime<Utc>,
    pub type_: String,
    pub interviewers: Value,
    pub notes: Option<String>,
    pub outcome: Option<String>,
    pub outcome_notes: Option<String>,
    pub status: String,
    pub scheduler_status: String,
    pub scheduler_job_id: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = interviews)]
pub struct NewInterview {
    pub id: Uuid,
    pub applicant_id: Uuid,
    pub job_id: Uuid,
    pub scheduled_at: DateTime<Utc>,
    pub notes: Option<String>,
    pub outcome: Option<String>,
    pub outcome_notes: Option<String>,
    pub scheduler_job_id: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = evaluations)]
pub struct Evaluation {
    pub id: Uuid,
    pub applicant_id: Uuid,
    pub job_id: Uuid,
    pub interviewer_id: String,
    pub interview_id: Option<Uuid>,
    pub scores: Value,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = evaluations)]
pub struct NewEvaluation {
    pub id: Uuid,
    pub applicant_id: Uuid,
    pub job_id: Uuid,
    pub interviewer_id: String,
    pub interview_id: Option<Uuid>,
    pub scores: Value,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = offers)]
pub struct Offer {
    pub id: Uuid,
    pub applicant_id: Uuid,
    pub job_id: Uuid,
    pub salary: PgNumeric,
    pub currency: String,
    pub start_date: Option<NaiveDate>,
    pub terms: Option<String>,
    pub status: String,
    pub created_by: String,
    pub sent_at: Option<DateTime<Utc>>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = offers)]
pub struct NewOffer {
    pub id: Uuid,
    pub applicant_id: Uuid,
    pub job_id: Uuid,
    pub salary: PgNumeric,
    pub start_date: Option<NaiveDate>,
    pub terms: Option<String>,
    pub created_by: String,
    pub sent_at: Option<DateTime<Utc>>,
    pub decided_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Document vault (migration 000052_vault).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = vault_folders)]
pub struct VaultFolder {
    pub id: Uuid,
    pub name: String,
    pub parent_id: Option<Uuid>,
    pub owner_id: String,
    pub company_id: Option<String>,
    pub retention_days: Option<i64>,
    pub auto_delete: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = vault_folders)]
pub struct NewVaultFolder {
    pub id: Uuid,
    pub name: String,
    pub parent_id: Option<Uuid>,
    pub owner_id: String,
    pub company_id: Option<String>,
    pub retention_days: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = vault_documents)]
pub struct VaultDocument {
    pub id: Uuid,
    pub folder_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub owner_id: String,
    pub company_id: Option<String>,
    pub current_version_id: Option<Uuid>,
    pub storage_target: String,
    pub storage_key: String,
    pub checksum_sha256: String,
    pub size_bytes: i64,
    pub mime_type: String,
    pub shared_with_client_ids: Vec<String>,
    pub status: String,
    pub flagged_for_review: bool,
    pub flagged_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = vault_documents)]
pub struct NewVaultDocument {
    pub id: Uuid,
    pub folder_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub owner_id: String,
    pub company_id: Option<String>,
    pub current_version_id: Option<Uuid>,
    pub storage_target: String,
    pub storage_key: String,
    pub checksum_sha256: String,
    pub flagged_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = vault_document_versions)]
pub struct VaultDocumentVersion {
    pub id: Uuid,
    pub document_id: Uuid,
    pub version_number: i64,
    pub storage_target: String,
    pub storage_key: String,
    pub checksum_sha256: String,
    pub size_bytes: i64,
    pub mime_type: String,
    pub uploaded_by: String,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = vault_document_versions)]
pub struct NewVaultDocumentVersion {
    pub id: Uuid,
    pub document_id: Uuid,
    pub version_number: i64,
    pub storage_target: String,
    pub storage_key: String,
    pub checksum_sha256: String,
    pub uploaded_by: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = vault_access_log)]
pub struct VaultAccessLog {
    pub id: Uuid,
    pub document_id: Uuid,
    pub actor_id: String,
    pub action: String,
    pub detail: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = vault_access_log)]
pub struct NewVaultAccessLog {
    pub id: Uuid,
    pub document_id: Uuid,
    pub actor_id: String,
    pub action: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = vault_share_links)]
pub struct VaultShareLink {
    pub id: Uuid,
    pub document_id: Uuid,
    pub token: String,
    pub expires_at: DateTime<Utc>,
    pub max_downloads: i64,
    pub download_count: i64,
    pub revoked: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = vault_share_links)]
pub struct NewVaultShareLink {
    pub id: Uuid,
    pub document_id: Uuid,
    pub token: String,
    pub expires_at: DateTime<Utc>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = vault_access_requests)]
pub struct VaultAccessRequest {
    pub id: Uuid,
    pub document_id: Uuid,
    pub requester_id: String,
    pub reason: Option<String>,
    pub status: String,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = vault_access_requests)]
pub struct NewVaultAccessRequest {
    pub id: Uuid,
    pub document_id: Uuid,
    pub requester_id: String,
    pub reason: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Incidents & on-call (migration 000053_incidents).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = incidents)]
pub struct Incident {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub severity: String,
    pub status: String,
    pub affected_service_ids: Vec<String>,
    pub responders: Vec<String>,
    pub current_responder: Option<String>,
    pub source: String,
    pub source_service_id: Option<String>,
    pub escalation_policy_id: Option<Uuid>,
    pub rotation_id: Option<Uuid>,
    pub created_by: String,
    pub acknowledged_by: Option<String>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = incidents)]
pub struct NewIncident {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub current_responder: Option<String>,
    pub source_service_id: Option<String>,
    pub escalation_policy_id: Option<Uuid>,
    pub rotation_id: Option<Uuid>,
    pub created_by: String,
    pub acknowledged_by: Option<String>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = incident_timeline)]
pub struct IncidentTimeline {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub actor_id: String,
    pub kind: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = incident_timeline)]
pub struct NewIncidentTimeline {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub actor_id: String,
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = on_call_rotations)]
pub struct OnCallRotation {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub members: Vec<String>,
    pub handover_interval_days: i64,
    pub handover_time: String,
    pub timezone: String,
    pub current_index: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = on_call_rotations)]
pub struct NewOnCallRotation {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = on_call_schedules)]
pub struct OnCallSchedule {
    pub id: Uuid,
    pub rotation_id: Uuid,
    pub member_id: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub kind: String,
    pub overrides_entry_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = on_call_schedules)]
pub struct NewOnCallSchedule {
    pub id: Uuid,
    pub rotation_id: Uuid,
    pub member_id: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub overrides_entry_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = escalation_policies)]
pub struct EscalationPolicy {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub applies_to_severities: Vec<String>,
    pub steps: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = escalation_policies)]
pub struct NewEscalationPolicy {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = incident_escalations)]
pub struct IncidentEscalation {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub policy_id: Uuid,
    pub step_index: i64,
    pub next_escalation_at: DateTime<Utc>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = incident_escalations)]
pub struct NewIncidentEscalation {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub policy_id: Uuid,
    pub next_escalation_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = page_attempts)]
pub struct PageAttempt {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub paged_user_id: String,
    pub channel: String,
    pub status: String,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = page_attempts)]
pub struct NewPageAttempt {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub paged_user_id: String,
    pub acknowledged_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = incident_postmortems)]
pub struct IncidentPostmortem {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub summary: String,
    pub timeline_summary: Option<String>,
    pub root_cause: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = incident_postmortems)]
pub struct NewIncidentPostmortem {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub summary: String,
    pub timeline_summary: Option<String>,
    pub root_cause: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = postmortem_action_items)]
pub struct PostmortemActionItem {
    pub id: Uuid,
    pub postmortem_id: Uuid,
    pub title: String,
    pub assignee_id: Option<String>,
    pub status: String,
    pub task_reference: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = postmortem_action_items)]
pub struct NewPostmortemActionItem {
    pub id: Uuid,
    pub postmortem_id: Uuid,
    pub title: String,
    pub assignee_id: Option<String>,
    pub task_reference: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = auto_open_rules)]
pub struct AutoOpenRule {
    pub service_id: String,
    pub enabled: bool,
    pub severity: String,
    pub auto_resolve_on_recovery: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = auto_open_rules)]
pub struct NewAutoOpenRule {
    pub service_id: String,
}

// ---------------------------------------------------------------------------
// Smart inbox (migration 000054_inbox).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = inbox_threads)]
pub struct InboxThread {
    pub id: Uuid,
    pub source_channel: String,
    pub source_id: String,
    pub subject: String,
    pub customer_id: Option<String>,
    pub customer_label: Option<String>,
    pub participants: Value,
    pub department_id: Option<String>,
    pub assignee_id: Option<String>,
    pub status: String,
    pub priority: String,
    pub unread: bool,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub snooze_scheduler_status: String,
    pub sla_due_at: Option<DateTime<Utc>>,
    pub sla_breached: bool,
    pub merged_into: Option<Uuid>,
    pub last_activity_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = inbox_threads)]
pub struct NewInboxThread {
    pub id: Uuid,
    pub source_channel: String,
    pub source_id: String,
    pub subject: String,
    pub customer_id: Option<String>,
    pub customer_label: Option<String>,
    pub department_id: Option<String>,
    pub assignee_id: Option<String>,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub sla_due_at: Option<DateTime<Utc>>,
    pub merged_into: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = inbox_assignments)]
pub struct InboxAssignment {
    pub id: Uuid,
    pub thread_id: Uuid,
    pub assignee_id: String,
    pub assigned_by: String,
    pub active: bool,
    pub assigned_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = inbox_assignments)]
pub struct NewInboxAssignment {
    pub id: Uuid,
    pub thread_id: Uuid,
    pub assignee_id: String,
    pub assigned_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = inbox_canned_replies)]
pub struct InboxCannedReply {
    pub id: Uuid,
    pub department_id: Option<String>,
    pub title: String,
    pub body: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = inbox_canned_replies)]
pub struct NewInboxCannedReply {
    pub id: Uuid,
    pub department_id: Option<String>,
    pub title: String,
    pub body: String,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = inbox_thread_reads)]
pub struct InboxThreadRead {
    pub thread_id: Uuid,
    pub user_id: String,
    pub read_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = inbox_thread_reads)]
pub struct NewInboxThreadRead {
    pub thread_id: Uuid,
    pub user_id: String,
}

// ---------------------------------------------------------------------------
// Surveys (migration 000055_surveys).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = surveys)]
pub struct Survey {
    pub id: Uuid,
    pub family_id: Uuid,
    pub version: i32,
    pub supersedes: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = surveys)]
pub struct NewSurvey {
    pub id: Uuid,
    pub family_id: Uuid,
    pub version: i32,
    pub supersedes: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = survey_questions)]
pub struct SurveyQuestion {
    pub id: Uuid,
    pub survey_id: Uuid,
    pub position: i32,
    pub question_type: String,
    pub text: String,
    pub required: bool,
    pub options: Value,
    pub min_value: Option<i32>,
    pub max_value: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = survey_questions)]
pub struct NewSurveyQuestion {
    pub id: Uuid,
    pub survey_id: Uuid,
    pub position: i32,
    pub question_type: String,
    pub text: String,
    pub min_value: Option<i32>,
    pub max_value: Option<i32>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = survey_distributions)]
pub struct SurveyDistribution {
    pub id: Uuid,
    pub survey_id: Uuid,
    pub trigger: String,
    pub department_id: Option<String>,
    pub product_id: Option<String>,
    pub agent_id: Option<String>,
    pub responsible_user_id: Option<String>,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub expires_in_days: i32,
    pub status: String,
    pub scheduler_status: String,
    pub scheduler_job_id: Option<String>,
    pub sent_count: i64,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = survey_distributions)]
pub struct NewSurveyDistribution {
    pub id: Uuid,
    pub survey_id: Uuid,
    pub trigger: String,
    pub department_id: Option<String>,
    pub product_id: Option<String>,
    pub agent_id: Option<String>,
    pub responsible_user_id: Option<String>,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub scheduler_job_id: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = survey_responses)]
pub struct SurveyResponse {
    pub id: Uuid,
    pub survey_id: Uuid,
    pub distribution_id: Option<Uuid>,
    pub recipient_id: String,
    pub recipient_email: Option<String>,
    pub agent_id: Option<String>,
    pub department_id: Option<String>,
    pub product_id: Option<String>,
    pub token: String,
    pub token_expires_at: DateTime<Utc>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub follow_up_status: String,
    pub follow_up_task_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = survey_responses)]
pub struct NewSurveyResponse {
    pub id: Uuid,
    pub survey_id: Uuid,
    pub distribution_id: Option<Uuid>,
    pub recipient_id: String,
    pub recipient_email: Option<String>,
    pub agent_id: Option<String>,
    pub department_id: Option<String>,
    pub product_id: Option<String>,
    pub token: String,
    pub token_expires_at: DateTime<Utc>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub follow_up_task_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = survey_answers)]
pub struct SurveyAnswer {
    pub id: Uuid,
    pub response_id: Uuid,
    pub question_id: Uuid,
    pub score: Option<i32>,
    pub text: Option<String>,
    pub choice: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = survey_answers)]
pub struct NewSurveyAnswer {
    pub id: Uuid,
    pub response_id: Uuid,
    pub question_id: Uuid,
    pub score: Option<i32>,
    pub text: Option<String>,
    pub choice: Option<String>,
}

// ---------------------------------------------------------------------------
// Community (migration 000056_community).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = community_categories)]
pub struct CommunityCategory {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub slug: String,
    pub sort_order: i64,
    pub visibility: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = community_categories)]
pub struct NewCommunityCategory {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub slug: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = community_topics)]
pub struct CommunityTopic {
    pub id: Uuid,
    pub category_id: Uuid,
    pub title: String,
    pub author_id: String,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub locked: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = community_topics)]
pub struct NewCommunityTopic {
    pub id: Uuid,
    pub category_id: Uuid,
    pub title: String,
    pub author_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = community_posts)]
pub struct CommunityPost {
    pub id: Uuid,
    pub topic_id: Uuid,
    pub parent_post_id: Option<Uuid>,
    pub author_id: String,
    pub body: String,
    pub hidden: bool,
    pub edited: bool,
    pub edited_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = community_posts)]
pub struct NewCommunityPost {
    pub id: Uuid,
    pub topic_id: Uuid,
    pub parent_post_id: Option<Uuid>,
    pub author_id: String,
    pub body: String,
    pub edited_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = community_post_edits)]
pub struct CommunityPostEdit {
    pub id: Uuid,
    pub post_id: Uuid,
    pub editor_id: String,
    pub previous_body: String,
    pub edited_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = community_post_edits)]
pub struct NewCommunityPostEdit {
    pub id: Uuid,
    pub post_id: Uuid,
    pub editor_id: String,
    pub previous_body: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = community_votes)]
pub struct CommunityVote {
    pub id: Uuid,
    pub post_id: Uuid,
    pub user_id: String,
    pub value: i16,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = community_votes)]
pub struct NewCommunityVote {
    pub id: Uuid,
    pub post_id: Uuid,
    pub user_id: String,
    pub value: i16,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = community_flags)]
pub struct CommunityFlag {
    pub id: Uuid,
    pub post_id: Uuid,
    pub reporter_id: String,
    pub reason: String,
    pub status: String,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = community_flags)]
pub struct NewCommunityFlag {
    pub id: Uuid,
    pub post_id: Uuid,
    pub reporter_id: String,
    pub reason: String,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = community_moderation_actions)]
pub struct CommunityModerationAction {
    pub id: Uuid,
    pub target_type: String,
    pub target_id: Uuid,
    pub action: String,
    pub moderator_id: String,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = community_moderation_actions)]
pub struct NewCommunityModerationAction {
    pub id: Uuid,
    pub target_type: String,
    pub target_id: Uuid,
    pub action: String,
    pub moderator_id: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = community_members)]
pub struct CommunityMember {
    pub id: Uuid,
    pub user_id: String,
    pub display_name: String,
    pub badge_count: i64,
    pub joined_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = community_members)]
pub struct NewCommunityMember {
    pub id: Uuid,
    pub user_id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = community_announcements)]
pub struct CommunityAnnouncement {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub pinned: bool,
    pub published: bool,
    pub published_at: Option<DateTime<Utc>>,
    pub author_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = community_announcements)]
pub struct NewCommunityAnnouncement {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub author_id: String,
    pub published_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Compliance (migration 000058_compliance).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = compliance_export_requests)]
pub struct ComplianceExportRequest {
    pub id: Uuid,
    pub requester_id: String,
    pub scope: Vec<String>,
    pub status: String,
    pub storage_target: Option<String>,
    pub storage_key: Option<String>,
    pub download_expires_at: Option<DateTime<Utc>>,
    pub failure_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = compliance_export_requests)]
pub struct NewComplianceExportRequest {
    pub id: Uuid,
    pub requester_id: String,
    pub storage_target: Option<String>,
    pub storage_key: Option<String>,
    pub download_expires_at: Option<DateTime<Utc>>,
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = compliance_deletion_requests)]
pub struct ComplianceDeletionRequest {
    pub id: Uuid,
    pub requester_id: String,
    pub reason: String,
    pub status: String,
    pub approved_by: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub executed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = compliance_deletion_requests)]
pub struct NewComplianceDeletionRequest {
    pub id: Uuid,
    pub requester_id: String,
    pub reason: String,
    pub approved_by: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub executed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = compliance_deletion_items)]
pub struct ComplianceDeletionItem {
    pub id: Uuid,
    pub deletion_request_id: Uuid,
    pub module_id: String,
    pub subject: String,
    pub status: String,
    pub detail: Option<String>,
    pub executed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = compliance_deletion_items)]
pub struct NewComplianceDeletionItem {
    pub id: Uuid,
    pub deletion_request_id: Uuid,
    pub module_id: String,
    pub subject: String,
    pub detail: Option<String>,
    pub executed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = compliance_consents)]
pub struct ComplianceConsent {
    pub id: Uuid,
    pub user_id: String,
    pub purpose: String,
    pub consent_text_version: String,
    pub granted: bool,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = compliance_consents)]
pub struct NewComplianceConsent {
    pub id: Uuid,
    pub user_id: String,
    pub purpose: String,
    pub consent_text_version: String,
    pub granted: bool,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = compliance_retention_policies)]
pub struct ComplianceRetentionPolicy {
    pub id: Uuid,
    pub data_category: String,
    pub retain_days: i64,
    pub review_action: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = compliance_retention_policies)]
pub struct NewComplianceRetentionPolicy {
    pub id: Uuid,
    pub data_category: String,
    pub retain_days: i64,
    pub review_action: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = compliance_retention_reviews)]
pub struct ComplianceRetentionReview {
    pub id: Uuid,
    pub policy_id: Uuid,
    pub items_flagged: i64,
    pub notes: Option<String>,
    pub scheduler_status: String,
    pub scheduler_job_id: Option<String>,
    pub reviewed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = compliance_retention_reviews)]
pub struct NewComplianceRetentionReview {
    pub id: Uuid,
    pub policy_id: Uuid,
    pub notes: Option<String>,
    pub scheduler_job_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = compliance_processing_records)]
pub struct ComplianceProcessingRecord {
    pub id: Uuid,
    pub purpose: String,
    pub data_categories: Vec<String>,
    pub recipients: Vec<String>,
    pub retention: String,
    pub lawful_basis: String,
    pub owner: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = compliance_processing_records)]
pub struct NewComplianceProcessingRecord {
    pub id: Uuid,
    pub purpose: String,
    pub retention: String,
    pub lawful_basis: String,
    pub owner: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = compliance_checklists)]
pub struct ComplianceChecklist {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub evidence_storage_target: Option<String>,
    pub evidence_storage_key: Option<String>,
    pub due_date: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = compliance_checklists)]
pub struct NewComplianceChecklist {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub evidence_storage_target: Option<String>,
    pub evidence_storage_key: Option<String>,
    pub due_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = compliance_breach_log)]
pub struct ComplianceBreachLog {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub discovered_at: DateTime<Utc>,
    pub affected_scope: String,
    pub status: String,
    pub notification_status: String,
    pub reported_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = compliance_breach_log)]
pub struct NewComplianceBreachLog {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub discovered_at: DateTime<Utc>,
    pub affected_scope: String,
    pub reported_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = compliance_breach_timeline)]
pub struct ComplianceBreachTimeline {
    pub id: Uuid,
    pub breach_id: Uuid,
    pub author_id: String,
    pub entry: String,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = compliance_breach_timeline)]
pub struct NewComplianceBreachTimeline {
    pub id: Uuid,
    pub breach_id: Uuid,
    pub author_id: String,
    pub entry: String,
}

// ---------------------------------------------------------------------------
// Gamification (migration 000059_gamification).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = gamification_rules)]
pub struct GamificationRule {
    pub id: Uuid,
    pub name: String,
    pub action: String,
    pub points: i64,
    pub company_id: Option<String>,
    pub daily_cap: Option<i64>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = gamification_rules)]
pub struct NewGamificationRule {
    pub id: Uuid,
    pub name: String,
    pub action: String,
    pub points: i64,
    pub company_id: Option<String>,
    pub daily_cap: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = gamification_points_ledger)]
pub struct GamificationPointsLedger {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<String>,
    pub department_id: Option<String>,
    pub rule_id: Option<Uuid>,
    pub action: String,
    pub points: i64,
    pub balance_after: i64,
    pub season_id: Option<Uuid>,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = gamification_points_ledger)]
pub struct NewGamificationPointsLedger {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<String>,
    pub department_id: Option<String>,
    pub rule_id: Option<Uuid>,
    pub action: String,
    pub points: i64,
    pub balance_after: i64,
    pub season_id: Option<Uuid>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = gamification_badges)]
pub struct GamificationBadge {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub criteria_action: String,
    pub criteria_threshold: i64,
    pub icon: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = gamification_badges)]
pub struct NewGamificationBadge {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub criteria_action: String,
    pub criteria_threshold: i64,
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = gamification_awards)]
pub struct GamificationAward {
    pub id: Uuid,
    pub badge_id: Uuid,
    pub user_id: String,
    pub reason: String,
    pub awarded_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = gamification_awards)]
pub struct NewGamificationAward {
    pub id: Uuid,
    pub badge_id: Uuid,
    pub user_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = gamification_seasons)]
pub struct GamificationSeason {
    pub id: Uuid,
    pub name: String,
    pub company_id: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = gamification_seasons)]
pub struct NewGamificationSeason {
    pub id: Uuid,
    pub name: String,
    pub company_id: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = gamification_rewards)]
pub struct GamificationReward {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub points_cost: i64,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = gamification_rewards)]
pub struct NewGamificationReward {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub points_cost: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = gamification_reward_requests)]
pub struct GamificationRewardRequest {
    pub id: Uuid,
    pub reward_id: Uuid,
    pub user_id: String,
    pub status: String,
    pub points_spent: i64,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = gamification_reward_requests)]
pub struct NewGamificationRewardRequest {
    pub id: Uuid,
    pub reward_id: Uuid,
    pub user_id: String,
    pub points_spent: i64,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Workflows (migration 000060_workflows).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = workflows)]
pub struct Workflow {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub trigger: Value,
    pub conditions: Option<Value>,
    pub actions: Value,
    pub is_active: bool,
    pub owner_id: String,
    pub company_id: Option<String>,
    pub webhook_token_hash: Option<String>,
    pub webhook_allowed_hosts: Value,
    pub run_count: i64,
    pub success_count: i64,
    pub failure_count: i64,
    pub dropped_count: i64,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_run_status: Option<String>,
    pub scheduler_status: String,
    pub scheduler_job_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = workflows)]
pub struct NewWorkflow {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub trigger: Value,
    pub conditions: Option<Value>,
    pub webhook_token_hash: Option<String>,
    pub owner_id: String,
    pub company_id: Option<String>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_run_status: Option<String>,
    pub scheduler_job_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = workflow_runs)]
pub struct WorkflowRun {
    pub id: Uuid,
    pub workflow_id: Uuid,
    pub trigger_kind: String,
    pub trigger_name: Option<String>,
    pub status: String,
    pub payload_summary: Value,
    pub dry_run: bool,
    pub depth: i32,
    pub condition_matched: bool,
    pub skipped_reason: Option<String>,
    pub retried_from_run_id: Option<Uuid>,
    pub duration_ms: Option<i64>,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = workflow_runs)]
pub struct NewWorkflowRun {
    pub id: Uuid,
    pub workflow_id: Uuid,
    pub trigger_kind: String,
    pub trigger_name: Option<String>,
    pub skipped_reason: Option<String>,
    pub retried_from_run_id: Option<Uuid>,
    pub duration_ms: Option<i64>,
    pub error: Option<String>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = workflow_run_steps)]
pub struct WorkflowRunStep {
    pub id: Uuid,
    pub run_id: Uuid,
    pub workflow_id: Uuid,
    pub step_index: i32,
    pub action_kind: String,
    pub status: String,
    pub attempts: i32,
    pub error: Option<String>,
    pub detail: Value,
    pub duration_ms: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = workflow_run_steps)]
pub struct NewWorkflowRunStep {
    pub id: Uuid,
    pub run_id: Uuid,
    pub workflow_id: Uuid,
    pub step_index: i32,
    pub action_kind: String,
    pub error: Option<String>,
    pub duration_ms: Option<i64>,
}

// ---------------------------------------------------------------------------
// Documents (migration 000065_documents).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = documents)]
pub struct Document {
    pub id: Uuid,
    pub name: String,
    pub module_id: String,
    pub company_id: Option<Uuid>,
    pub owner_id: String,
    pub scope: String,
    pub mime: String,
    pub size: i64,
    pub checksum: String,
    pub storage_key: String,
    pub current_version: i32,
    pub version_count: i32,
    pub scan_status: String,
    pub scan_threat: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = documents)]
pub struct NewDocument {
    pub id: Uuid,
    pub name: String,
    pub module_id: String,
    pub company_id: Option<Uuid>,
    pub owner_id: String,
    pub mime: String,
    pub size: i64,
    pub checksum: String,
    pub storage_key: String,
    pub scan_threat: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = document_versions)]
pub struct DocumentVersion {
    pub id: Uuid,
    pub document_id: Uuid,
    pub version: i32,
    pub checksum: String,
    pub storage_key: String,
    pub size: i64,
    pub mime: String,
    pub uploader_id: String,
    pub scan_status: String,
    pub scan_threat: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = document_versions)]
pub struct NewDocumentVersion {
    pub id: Uuid,
    pub document_id: Uuid,
    pub version: i32,
    pub checksum: String,
    pub storage_key: String,
    pub size: i64,
    pub mime: String,
    pub uploader_id: String,
    pub scan_threat: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = document_upload_sessions)]
pub struct DocumentUploadSession {
    pub id: Uuid,
    pub document_id: Option<Uuid>,
    pub name: String,
    pub module_id: String,
    pub company_id: Option<Uuid>,
    pub owner_id: String,
    pub total_size: i64,
    pub expected_sha256: String,
    pub chunk_size: i64,
    pub part_count: i32,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = document_upload_sessions)]
pub struct NewDocumentUploadSession {
    pub id: Uuid,
    pub document_id: Option<Uuid>,
    pub name: String,
    pub module_id: String,
    pub company_id: Option<Uuid>,
    pub owner_id: String,
    pub total_size: i64,
    pub expected_sha256: String,
    pub chunk_size: i64,
    pub part_count: i32,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = document_upload_parts)]
pub struct DocumentUploadPart {
    pub id: Uuid,
    pub session_id: Uuid,
    pub part_index: i32,
    pub size: i64,
    pub part_sha256: String,
    pub received_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = document_upload_parts)]
pub struct NewDocumentUploadPart {
    pub id: Uuid,
    pub session_id: Uuid,
    pub part_index: i32,
    pub size: i64,
    pub part_sha256: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = document_allowlist_rules)]
pub struct DocumentAllowlistRule {
    pub id: Uuid,
    pub module_id: String,
    pub extensions: Vec<String>,
    pub mime_types: Vec<String>,
    pub max_bytes: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = document_allowlist_rules)]
pub struct NewDocumentAllowlistRule {
    pub id: Uuid,
    pub module_id: String,
    pub max_bytes: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = document_downloads)]
pub struct DocumentDownload {
    pub id: Uuid,
    pub document_id: Option<Uuid>,
    pub version: i32,
    pub user_id: String,
    pub via: String,
    pub scan_status: String,
    pub bytes: i64,
    pub downloaded_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = document_downloads)]
pub struct NewDocumentDownload {
    pub id: Uuid,
    pub document_id: Option<Uuid>,
    pub version: i32,
    pub user_id: String,
    pub scan_status: String,
    pub bytes: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = document_quotas)]
pub struct DocumentQuota {
    pub id: Uuid,
    pub scope: String,
    pub scope_id: String,
    pub bytes_limit: Option<i64>,
    pub used_bytes: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = document_quotas)]
pub struct NewDocumentQuota {
    pub id: Uuid,
    pub scope: String,
    pub scope_id: String,
    pub bytes_limit: Option<i64>,
}

// ---------------------------------------------------------------------------
// Privacy (migration 000066_privacy).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = privacy_field_declarations)]
pub struct PrivacyFieldDeclaration {
    pub id: Uuid,
    pub module_id: String,
    pub field_name: String,
    pub purpose: String,
    pub required: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = privacy_field_declarations)]
pub struct NewPrivacyFieldDeclaration {
    pub id: Uuid,
    pub module_id: String,
    pub field_name: String,
    pub purpose: String,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = privacy_anonymizations)]
pub struct PrivacyAnonymization {
    pub id: Uuid,
    pub user_id: String,
    pub token: String,
    pub field_tokens: Value,
    pub scope_modules: Vec<String>,
    pub reason: Option<String>,
    pub actor: String,
    pub times_applied: i64,
    pub anonymized_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = privacy_anonymizations)]
pub struct NewPrivacyAnonymization {
    pub id: Uuid,
    pub user_id: String,
    pub token: String,
    pub reason: Option<String>,
    pub actor: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = privacy_visibility_defaults)]
pub struct PrivacyVisibilityDefault {
    pub id: Uuid,
    pub module_id: String,
    pub visibility: String,
    pub explicit: bool,
    pub configured_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = privacy_visibility_defaults)]
pub struct NewPrivacyVisibilityDefault {
    pub id: Uuid,
    pub module_id: String,
    pub configured_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = privacy_consents)]
pub struct PrivacyConsent {
    pub id: Uuid,
    pub user_id: String,
    pub consent_type: String,
    pub granted: bool,
    pub granted_at: Option<DateTime<Utc>>,
    pub withdrawn_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub source: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = privacy_consents)]
pub struct NewPrivacyConsent {
    pub id: Uuid,
    pub user_id: String,
    pub consent_type: String,
    pub granted_at: Option<DateTime<Utc>>,
    pub withdrawn_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = privacy_consent_history)]
pub struct PrivacyConsentHistory {
    pub id: Uuid,
    pub consent_id: Uuid,
    pub user_id: String,
    pub consent_type: String,
    pub action: String,
    pub actor: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = privacy_consent_history)]
pub struct NewPrivacyConsentHistory {
    pub id: Uuid,
    pub consent_id: Uuid,
    pub user_id: String,
    pub consent_type: String,
    pub action: String,
    pub actor: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = privacy_erasure_requests)]
pub struct PrivacyErasureRequest {
    pub id: Uuid,
    pub user_id: String,
    pub modules: Vec<String>,
    pub reason: String,
    pub status: String,
    pub requested_by: String,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub rejection_reason: Option<String>,
    pub fulfilled_at: Option<DateTime<Utc>>,
    pub anonymization_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = privacy_erasure_requests)]
pub struct NewPrivacyErasureRequest {
    pub id: Uuid,
    pub user_id: String,
    pub reason: String,
    pub requested_by: String,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub rejection_reason: Option<String>,
    pub fulfilled_at: Option<DateTime<Utc>>,
    pub anonymization_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = privacy_notices)]
pub struct PrivacyNotice {
    pub id: Uuid,
    pub module_id: String,
    pub text: String,
    pub version: i64,
    pub effective_from: DateTime<Utc>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = privacy_notices)]
pub struct NewPrivacyNotice {
    pub id: Uuid,
    pub module_id: String,
    pub text: String,
    pub version: i64,
    pub effective_from: DateTime<Utc>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = privacy_audit_entries)]
pub struct PrivacyAuditEntry {
    pub id: Uuid,
    pub actor: String,
    pub action: String,
    pub resource: String,
    pub details: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = privacy_audit_entries)]
pub struct NewPrivacyAuditEntry {
    pub id: Uuid,
    pub actor: String,
    pub action: String,
    pub resource: String,
}

// ---------------------------------------------------------------------------
// Web3 (migration 000067_web3).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = web3_networks)]
pub struct Web3Network {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub endpoint_url: String,
    pub chain_id: Option<i64>,
    pub native_asset_symbol: String,
    pub explorer_url_template: Option<String>,
    pub enabled: bool,
    pub indexer_url: Option<String>,
    pub verify_endpoint: Option<String>,
    #[serde(skip_serializing)]
    pub api_key_encrypted: Option<String>,
    pub allow_private_endpoint: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = web3_networks)]
pub struct NewWeb3Network {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub endpoint_url: String,
    pub chain_id: Option<i64>,
    pub native_asset_symbol: String,
    pub explorer_url_template: Option<String>,
    pub indexer_url: Option<String>,
    pub verify_endpoint: Option<String>,
    pub api_key_encrypted: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = web3_watched_addresses)]
pub struct Web3WatchedAddress {
    pub id: Uuid,
    pub network_id: Uuid,
    pub address: String,
    pub label: Option<String>,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = web3_watched_addresses)]
pub struct NewWeb3WatchedAddress {
    pub id: Uuid,
    pub network_id: Uuid,
    pub address: String,
    pub label: Option<String>,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub last_checked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = web3_transactions)]
pub struct Web3Transaction {
    pub id: Uuid,
    pub network_id: Uuid,
    pub tx_hash: String,
    pub from_address: String,
    pub to_address: String,
    pub amount_raw: String,
    pub amount: Option<PgNumeric>,
    pub asset: String,
    pub status: String,
    pub confirmations: i64,
    pub block_number: Option<i64>,
    pub direction: String,
    pub watched_address_id: Option<Uuid>,
    pub occurred_at: Option<DateTime<Utc>>,
    pub first_seen_at: DateTime<Utc>,
    pub last_checked_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = web3_transactions)]
pub struct NewWeb3Transaction {
    pub id: Uuid,
    pub network_id: Uuid,
    pub tx_hash: String,
    pub from_address: String,
    pub to_address: String,
    pub amount_raw: String,
    pub amount: Option<PgNumeric>,
    pub asset: String,
    pub watched_address_id: Option<Uuid>,
    pub occurred_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = web3_anchors)]
pub struct Web3Anchor {
    pub id: Uuid,
    pub record_type: String,
    pub record_id: String,
    pub content_sha256: String,
    pub network_id: Uuid,
    pub status: String,
    pub tx_hash: Option<String>,
    pub confirmations: i64,
    pub block_number: Option<i64>,
    pub requested_by: String,
    pub company_id: Option<Uuid>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = web3_anchors)]
pub struct NewWeb3Anchor {
    pub id: Uuid,
    pub record_type: String,
    pub record_id: String,
    pub content_sha256: String,
    pub network_id: Uuid,
    pub tx_hash: Option<String>,
    pub requested_by: String,
    pub company_id: Option<Uuid>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub confirmed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = web3_wallets)]
pub struct Web3Wallet {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub network_id: Uuid,
    pub address: String,
    pub label: Option<String>,
    pub verification_status: String,
    pub challenge_nonce_hash: Option<String>,
    pub challenge_expires_at: Option<DateTime<Utc>>,
    pub verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = web3_wallets)]
pub struct NewWeb3Wallet {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub network_id: Uuid,
    pub address: String,
    pub label: Option<String>,
    pub challenge_nonce_hash: Option<String>,
    pub challenge_expires_at: Option<DateTime<Utc>>,
    pub verified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = web3_balances)]
pub struct Web3Balance {
    pub id: Uuid,
    pub wallet_id: Uuid,
    pub network_id: Uuid,
    pub address: String,
    pub asset: String,
    pub amount_raw: String,
    pub amount: Option<PgNumeric>,
    pub decimals: i32,
    pub checked_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = web3_balances)]
pub struct NewWeb3Balance {
    pub id: Uuid,
    pub wallet_id: Uuid,
    pub network_id: Uuid,
    pub address: String,
    pub asset: String,
    pub amount_raw: String,
    pub amount: Option<PgNumeric>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = web3_settings)]
pub struct Web3Setting {
    pub id: Uuid,
    pub scope: String,
    pub public_ledger_enabled: bool,
    pub allowed_private_hosts: Vec<String>,
    pub allowed_extra_hosts: Vec<String>,
    pub anchoring_relay_url: Option<String>,
    #[serde(skip_serializing)]
    pub anchoring_key_encrypted: Option<String>,
    pub max_poll_transactions: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = web3_settings)]
pub struct NewWeb3Setting {
    pub id: Uuid,
    pub anchoring_relay_url: Option<String>,
    pub anchoring_key_encrypted: Option<String>,
}

// ---------------------------------------------------------------------------
// Point of sale (migration 000076_pos).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = pos_registers)]
pub struct PosRegister {
    pub id: Uuid,
    pub name: String,
    pub code: String,
    pub location: Option<String>,
    pub status: String,
    pub cashiers: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = pos_registers)]
pub struct NewPosRegister {
    pub id: Uuid,
    pub name: String,
    pub code: String,
    pub location: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pos_shifts)]
pub struct PosShift {
    pub id: Uuid,
    pub register_id: Uuid,
    pub cashier_id: String,
    pub status: String,
    pub opening_float: PgNumeric,
    pub currency: String,
    pub expected_cash: Option<PgNumeric>,
    pub counted_cash: Option<PgNumeric>,
    pub variance: Option<PgNumeric>,
    pub notes: Option<String>,
    pub opened_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pos_shifts)]
pub struct NewPosShift {
    pub id: Uuid,
    pub register_id: Uuid,
    pub cashier_id: String,
    pub expected_cash: Option<PgNumeric>,
    pub counted_cash: Option<PgNumeric>,
    pub variance: Option<PgNumeric>,
    pub notes: Option<String>,
    pub closed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pos_sales)]
pub struct PosSale {
    pub id: Uuid,
    pub number: String,
    pub register_id: Uuid,
    pub shift_id: Option<Uuid>,
    pub cashier_id: String,
    pub company_id: Option<String>,
    pub discount_type: Option<String>,
    pub discount_value: PgNumeric,
    pub discount_total: PgNumeric,
    pub subtotal: PgNumeric,
    pub tax_total: PgNumeric,
    pub total: PgNumeric,
    pub currency: String,
    pub tax_inclusive: bool,
    pub status: String,
    pub amount_paid: PgNumeric,
    pub change_due: PgNumeric,
    pub refunded_amount: PgNumeric,
    pub customer_name: Option<String>,
    pub notes: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub voided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pos_sales)]
pub struct NewPosSale {
    pub id: Uuid,
    pub number: String,
    pub register_id: Uuid,
    pub shift_id: Option<Uuid>,
    pub cashier_id: String,
    pub company_id: Option<String>,
    pub discount_type: Option<String>,
    pub customer_name: Option<String>,
    pub notes: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub voided_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pos_sale_items)]
pub struct PosSaleItem {
    pub id: Uuid,
    pub sale_id: Uuid,
    pub product_id: String,
    pub name: String,
    pub barcode: Option<String>,
    pub sku: Option<String>,
    pub unit_price: PgNumeric,
    pub quantity: i32,
    pub refunded_quantity: i32,
    pub discount_type: Option<String>,
    pub discount_value: PgNumeric,
    pub discount_amount: PgNumeric,
    pub line_subtotal: PgNumeric,
    pub tax_rate: PgNumeric,
    pub tax_amount: PgNumeric,
    pub line_total: PgNumeric,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pos_sale_items)]
pub struct NewPosSaleItem {
    pub id: Uuid,
    pub sale_id: Uuid,
    pub product_id: String,
    pub name: String,
    pub barcode: Option<String>,
    pub sku: Option<String>,
    pub discount_type: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pos_payments)]
pub struct PosPayment {
    pub id: Uuid,
    pub sale_id: Uuid,
    pub method_id: String,
    pub method_code: String,
    pub amount: PgNumeric,
    pub tendered: PgNumeric,
    pub reference: Option<String>,
    pub recorded_by: String,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pos_payments)]
pub struct NewPosPayment {
    pub id: Uuid,
    pub sale_id: Uuid,
    pub method_id: String,
    pub method_code: String,
    pub amount: PgNumeric,
    pub tendered: PgNumeric,
    pub reference: Option<String>,
    pub recorded_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pos_refunds)]
pub struct PosRefund {
    pub id: Uuid,
    pub sale_id: Uuid,
    pub shift_id: Option<Uuid>,
    pub amount: PgNumeric,
    pub method_id: String,
    pub method_code: String,
    pub items: Value,
    pub reason: String,
    pub refunded_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pos_refunds)]
pub struct NewPosRefund {
    pub id: Uuid,
    pub sale_id: Uuid,
    pub shift_id: Option<Uuid>,
    pub amount: PgNumeric,
    pub method_id: String,
    pub method_code: String,
    pub reason: String,
    pub refunded_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pos_receipts)]
pub struct PosReceipt {
    pub id: Uuid,
    pub number: String,
    pub sale_id: Uuid,
    pub sale_number: String,
    pub register_id: Uuid,
    pub register_code: Option<String>,
    pub cashier_id: String,
    pub store_name: String,
    pub store_address: Option<String>,
    pub subtotal: PgNumeric,
    pub discount_total: PgNumeric,
    pub tax_total: PgNumeric,
    pub total: PgNumeric,
    pub currency: String,
    pub tax_breakdown: Value,
    pub amount_paid: PgNumeric,
    pub change_due: PgNumeric,
    pub print_lines: Value,
    pub footer: Option<String>,
    pub issued_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pos_receipts)]
pub struct NewPosReceipt {
    pub id: Uuid,
    pub number: String,
    pub sale_id: Uuid,
    pub sale_number: String,
    pub register_id: Uuid,
    pub register_code: Option<String>,
    pub cashier_id: String,
    pub store_name: String,
    pub store_address: Option<String>,
    pub footer: Option<String>,
}

// ---------------------------------------------------------------------------
// Appointments (migration 000077_appointments).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = appointment_services)]
pub struct AppointmentService {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub duration_minutes: i32,
    pub price: PgNumeric,
    pub currency: String,
    pub capacity: i32,
    pub requires_staff: bool,
    pub location_type: String,
    pub location: Option<String>,
    pub buffer_before_minutes: Option<i32>,
    pub buffer_after_minutes: Option<i32>,
    pub cancellation_notice_hours: Option<i32>,
    pub cancellation_fee_percent: Option<PgNumeric>,
    pub is_active: bool,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = appointment_services)]
pub struct NewAppointmentService {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub duration_minutes: i32,
    pub location: Option<String>,
    pub buffer_before_minutes: Option<i32>,
    pub buffer_after_minutes: Option<i32>,
    pub cancellation_notice_hours: Option<i32>,
    pub cancellation_fee_percent: Option<PgNumeric>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = appointment_staff_availability)]
pub struct AppointmentStaffAvailability {
    pub id: Uuid,
    pub staff_user_id: String,
    pub kind: String,
    pub weekday: Option<i32>,
    pub date: Option<NaiveDate>,
    pub start_minutes: Option<i32>,
    pub end_minutes: Option<i32>,
    pub break_start_minutes: Option<i32>,
    pub break_end_minutes: Option<i32>,
    pub valid_from: Option<NaiveDate>,
    pub valid_until: Option<NaiveDate>,
    pub reason: Option<String>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = appointment_staff_availability)]
pub struct NewAppointmentStaffAvailability {
    pub id: Uuid,
    pub staff_user_id: String,
    pub kind: String,
    pub weekday: Option<i32>,
    pub date: Option<NaiveDate>,
    pub start_minutes: Option<i32>,
    pub end_minutes: Option<i32>,
    pub break_start_minutes: Option<i32>,
    pub break_end_minutes: Option<i32>,
    pub valid_from: Option<NaiveDate>,
    pub valid_until: Option<NaiveDate>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = appointment_staff_services)]
pub struct AppointmentStaffService {
    pub id: Uuid,
    pub staff_user_id: String,
    pub service_id: Uuid,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = appointment_staff_services)]
pub struct NewAppointmentStaffService {
    pub id: Uuid,
    pub staff_user_id: String,
    pub service_id: Uuid,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = appointments)]
pub struct Appointment {
    pub id: Uuid,
    pub service_id: Uuid,
    pub staff_user_id: Option<String>,
    pub customer_user_id: Option<String>,
    pub customer_name: String,
    pub customer_email: String,
    pub customer_phone: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub status: String,
    pub source: String,
    pub price: PgNumeric,
    pub currency: String,
    pub cancellation_fee_charged: Option<PgNumeric>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
    pub notes: Option<String>,
    pub confirmation_code: String,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = appointments)]
pub struct NewAppointment {
    pub id: Uuid,
    pub service_id: Uuid,
    pub staff_user_id: Option<String>,
    pub customer_user_id: Option<String>,
    pub customer_name: String,
    pub customer_email: String,
    pub customer_phone: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub cancellation_fee_charged: Option<PgNumeric>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
    pub notes: Option<String>,
    pub confirmation_code: String,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = appointment_reminders)]
pub struct AppointmentReminder {
    pub id: Uuid,
    pub appointment_id: Uuid,
    pub offset_minutes: i32,
    pub channels: Vec<String>,
    pub status: String,
    pub scheduler_job_id: Option<String>,
    pub scheduler_status: String,
    pub sent_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = appointment_reminders)]
pub struct NewAppointmentReminder {
    pub id: Uuid,
    pub appointment_id: Uuid,
    pub offset_minutes: i32,
    pub scheduler_job_id: Option<String>,
    pub sent_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Digital products (migration 000078_digital_products).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = digital_product_types)]
pub struct DigitalProductType {
    pub id: String,
    pub label: String,
    pub description: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = digital_product_types)]
pub struct NewDigitalProductType {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = digital_products)]
pub struct DigitalProduct {
    pub id: Uuid,
    pub product_id: String,
    pub type_id: String,
    pub author: Option<String>,
    pub publisher: Option<String>,
    pub edition: Option<String>,
    pub isbn: Option<String>,
    pub language: Option<String>,
    pub pages: Option<i32>,
    pub formats: Vec<String>,
    pub cover_storage_id: Option<String>,
    pub sample_allowed: bool,
    pub download_limit: i32,
    pub royalty_kind: String,
    pub royalty_value: PgNumeric,
    pub royalty_payee: Option<String>,
    pub royalty_payee_company_id: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = digital_products)]
pub struct NewDigitalProduct {
    pub id: Uuid,
    pub product_id: String,
    pub type_id: String,
    pub author: Option<String>,
    pub publisher: Option<String>,
    pub edition: Option<String>,
    pub isbn: Option<String>,
    pub language: Option<String>,
    pub pages: Option<i32>,
    pub cover_storage_id: Option<String>,
    pub royalty_payee: Option<String>,
    pub royalty_payee_company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = digital_product_files)]
pub struct DigitalProductFile {
    pub id: Uuid,
    pub digital_product_id: Uuid,
    pub storage_file_id: String,
    pub format: String,
    pub label: Option<String>,
    pub file_size_bytes: Option<i64>,
    pub checksum: Option<String>,
    pub is_primary: bool,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = digital_product_files)]
pub struct NewDigitalProductFile {
    pub id: Uuid,
    pub digital_product_id: Uuid,
    pub storage_file_id: String,
    pub format: String,
    pub label: Option<String>,
    pub file_size_bytes: Option<i64>,
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = digital_product_previews)]
pub struct DigitalProductPreview {
    pub id: Uuid,
    pub digital_product_id: Uuid,
    pub storage_file_id: String,
    pub format: String,
    pub label: Option<String>,
    pub allowed: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = digital_product_previews)]
pub struct NewDigitalProductPreview {
    pub id: Uuid,
    pub digital_product_id: Uuid,
    pub storage_file_id: String,
    pub format: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = digital_payout_requests)]
pub struct DigitalPayoutRequest {
    pub id: Uuid,
    pub payee: Option<String>,
    pub payee_company_id: Option<String>,
    pub currency: String,
    pub amount: PgNumeric,
    pub royalty_ids: Vec<Uuid>,
    pub status: String,
    pub requested_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = digital_payout_requests)]
pub struct NewDigitalPayoutRequest {
    pub id: Uuid,
    pub payee: Option<String>,
    pub payee_company_id: Option<String>,
    pub currency: String,
    pub amount: PgNumeric,
    pub requested_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = digital_royalties)]
pub struct DigitalRoyalty {
    pub id: Uuid,
    pub digital_product_id: Uuid,
    pub product_id: String,
    pub order_id: String,
    pub buyer_user_id: String,
    pub payee: Option<String>,
    pub payee_company_id: Option<String>,
    pub amount: PgNumeric,
    pub currency: String,
    pub royalty_kind: String,
    pub royalty_value: PgNumeric,
    pub status: String,
    pub payout_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = digital_royalties)]
pub struct NewDigitalRoyalty {
    pub id: Uuid,
    pub digital_product_id: Uuid,
    pub product_id: String,
    pub order_id: String,
    pub buyer_user_id: String,
    pub payee: Option<String>,
    pub payee_company_id: Option<String>,
    pub amount: PgNumeric,
    pub currency: String,
    pub royalty_kind: String,
    pub royalty_value: PgNumeric,
    pub payout_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = digital_downloads)]
pub struct DigitalDownload {
    pub id: Uuid,
    pub digital_product_id: Uuid,
    pub file_id: Option<Uuid>,
    pub preview_id: Option<Uuid>,
    pub buyer_user_id: String,
    pub order_id: Option<String>,
    pub grant_id: Option<String>,
    pub kind: String,
    pub watermark_subject: Option<String>,
    pub watermark_applied: bool,
    pub watermark_formats: Vec<String>,
    pub downloaded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = digital_downloads)]
pub struct NewDigitalDownload {
    pub id: Uuid,
    pub digital_product_id: Uuid,
    pub file_id: Option<Uuid>,
    pub preview_id: Option<Uuid>,
    pub buyer_user_id: String,
    pub order_id: Option<String>,
    pub grant_id: Option<String>,
    pub kind: String,
    pub watermark_subject: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = digital_watermark_formats)]
pub struct DigitalWatermarkFormat {
    pub format: String,
    pub metadata_supported: bool,
    pub metadata_fields: Vec<String>,
    pub notes: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = digital_watermark_formats)]
pub struct NewDigitalWatermarkFormat {
    pub format: String,
}

// ---------------------------------------------------------------------------
// E-learning (migration 000079_elearning; `certificates` omitted —
// see the schema note above).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = courses)]
pub struct Course {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub category: String,
    pub level: String,
    pub language: String,
    pub thumbnail_ref: Option<String>,
    pub instructor_id: String,
    pub status: String,
    pub price_minor: i64,
    pub currency: String,
    pub certificate_enabled: bool,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = courses)]
pub struct NewCourse {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub category: String,
    pub level: String,
    pub language: String,
    pub thumbnail_ref: Option<String>,
    pub instructor_id: String,
    pub status: String,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = course_modules)]
pub struct CourseModule {
    pub id: Uuid,
    pub course_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub position: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = course_modules)]
pub struct NewCourseModule {
    pub id: Uuid,
    pub course_id: Uuid,
    pub title: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = lessons)]
pub struct Lesson {
    pub id: Uuid,
    pub module_id: Uuid,
    pub course_id: Uuid,
    pub title: String,
    pub lesson_type: String,
    pub content_ref: Option<String>,
    pub body: Option<String>,
    pub position: i64,
    pub required: bool,
    pub available_from: Option<DateTime<Utc>>,
    pub requires_previous: bool,
    pub duration_minutes: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = lessons)]
pub struct NewLesson {
    pub id: Uuid,
    pub module_id: Uuid,
    pub course_id: Uuid,
    pub title: String,
    pub lesson_type: String,
    pub content_ref: Option<String>,
    pub body: Option<String>,
    pub available_from: Option<DateTime<Utc>>,
    pub duration_minutes: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = enrollments)]
pub struct Enrollment {
    pub id: Uuid,
    pub course_id: Uuid,
    pub user_id: String,
    pub status: String,
    pub price_paid_minor: i64,
    pub currency: String,
    pub order_id: Option<String>,
    pub learner_name: Option<String>,
    pub enrolled_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = enrollments)]
pub struct NewEnrollment {
    pub id: Uuid,
    pub course_id: Uuid,
    pub user_id: String,
    pub status: String,
    pub order_id: Option<String>,
    pub learner_name: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = lesson_progress)]
pub struct LessonProgress {
    pub id: Uuid,
    pub enrollment_id: Uuid,
    pub lesson_id: Uuid,
    pub course_id: Uuid,
    pub user_id: String,
    pub completed: bool,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = lesson_progress)]
pub struct NewLessonProgress {
    pub id: Uuid,
    pub enrollment_id: Uuid,
    pub lesson_id: Uuid,
    pub course_id: Uuid,
    pub user_id: String,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = quizzes)]
pub struct Quiz {
    pub id: Uuid,
    pub lesson_id: Uuid,
    pub course_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub pass_mark: i64,
    pub max_attempts: Option<i64>,
    pub allow_review: bool,
    pub time_limit_minutes: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = quizzes)]
pub struct NewQuiz {
    pub id: Uuid,
    pub lesson_id: Uuid,
    pub course_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub max_attempts: Option<i64>,
    pub time_limit_minutes: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = quiz_questions)]
pub struct QuizQuestion {
    pub id: Uuid,
    pub quiz_id: Uuid,
    pub question: String,
    pub options: Value,
    pub correct_index: i64,
    pub points: i64,
    pub position: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = quiz_questions)]
pub struct NewQuizQuestion {
    pub id: Uuid,
    pub quiz_id: Uuid,
    pub question: String,
    pub options: Value,
    pub correct_index: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = quiz_attempts)]
pub struct QuizAttempt {
    pub id: Uuid,
    pub quiz_id: Uuid,
    pub lesson_id: Uuid,
    pub enrollment_id: Uuid,
    pub user_id: String,
    pub answers: Value,
    pub score: i64,
    pub max_score: i64,
    pub percent: i64,
    pub passed: bool,
    pub submitted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = quiz_attempts)]
pub struct NewQuizAttempt {
    pub id: Uuid,
    pub quiz_id: Uuid,
    pub lesson_id: Uuid,
    pub enrollment_id: Uuid,
    pub user_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = reviews)]
pub struct Review {
    pub id: Uuid,
    pub course_id: Uuid,
    pub user_id: String,
    pub rating: i64,
    pub comment: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = reviews)]
pub struct NewReview {
    pub id: Uuid,
    pub course_id: Uuid,
    pub user_id: String,
    pub rating: i64,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = learning_paths)]
pub struct LearningPath {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = learning_paths)]
pub struct NewLearningPath {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = learning_path_courses)]
pub struct LearningPathCourse {
    pub id: Uuid,
    pub path_id: Uuid,
    pub course_id: Uuid,
    pub position: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = learning_path_courses)]
pub struct NewLearningPathCourse {
    pub id: Uuid,
    pub path_id: Uuid,
    pub course_id: Uuid,
}

// ---------------------------------------------------------------------------
// Field service (migration 000080_field_service).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = field_equipment)]
pub struct FieldEquipment {
    pub id: Uuid,
    pub asset_id: Option<Uuid>,
    pub equipment_type_id: Uuid,
    pub name: String,
    pub serial_number: Option<String>,
    pub location: Option<String>,
    pub install_date: Option<NaiveDate>,
    pub warranty_end: Option<NaiveDate>,
    pub status: String,
    pub usage_reading: Option<f64>,
    pub usage_unit: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = field_equipment)]
pub struct NewFieldEquipment {
    pub id: Uuid,
    pub asset_id: Option<Uuid>,
    pub equipment_type_id: Uuid,
    pub name: String,
    pub serial_number: Option<String>,
    pub location: Option<String>,
    pub install_date: Option<NaiveDate>,
    pub warranty_end: Option<NaiveDate>,
    pub status: String,
    pub usage_reading: Option<f64>,
    pub usage_unit: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = field_maintenance_plans)]
pub struct FieldMaintenancePlan {
    pub id: Uuid,
    pub equipment_id: Uuid,
    pub template_id: Option<Uuid>,
    pub name: String,
    pub description: Option<String>,
    pub interval_value: i64,
    pub interval_unit: String,
    pub last_service_at: Option<DateTime<Utc>>,
    pub usage_at_last_service: Option<f64>,
    pub next_due_at: Option<DateTime<Utc>>,
    pub next_due_usage: Option<f64>,
    pub is_active: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = field_maintenance_plans)]
pub struct NewFieldMaintenancePlan {
    pub id: Uuid,
    pub equipment_id: Uuid,
    pub template_id: Option<Uuid>,
    pub name: String,
    pub description: Option<String>,
    pub interval_value: i64,
    pub interval_unit: String,
    pub last_service_at: Option<DateTime<Utc>>,
    pub usage_at_last_service: Option<f64>,
    pub next_due_at: Option<DateTime<Utc>>,
    pub next_due_usage: Option<f64>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = field_work_orders)]
pub struct FieldWorkOrder {
    pub id: Uuid,
    pub equipment_id: Uuid,
    pub plan_id: Option<Uuid>,
    pub project_id: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub order_type: String,
    pub priority: String,
    pub status: String,
    pub assignee_id: Option<String>,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub due_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub verified_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = field_work_orders)]
pub struct NewFieldWorkOrder {
    pub id: Uuid,
    pub equipment_id: Uuid,
    pub plan_id: Option<Uuid>,
    pub project_id: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub order_type: String,
    pub priority: String,
    pub status: String,
    pub assignee_id: Option<String>,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub due_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub verified_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = field_work_order_items)]
pub struct FieldWorkOrderItem {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub title: String,
    pub position: i64,
    pub is_done: bool,
    pub done_by: Option<String>,
    pub done_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = field_work_order_items)]
pub struct NewFieldWorkOrderItem {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub title: String,
    pub done_by: Option<String>,
    pub done_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = field_work_order_parts)]
pub struct FieldWorkOrderPart {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub stock_id: Option<Uuid>,
    pub part_name: String,
    pub quantity: i64,
    pub unit: String,
    pub status: String,
    pub consumed_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = field_work_order_parts)]
pub struct NewFieldWorkOrderPart {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub stock_id: Option<Uuid>,
    pub part_name: String,
    pub quantity: i64,
    pub consumed_at: Option<DateTime<Utc>>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = field_dispatch_assignments)]
pub struct FieldDispatchAssignment {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub technician_id: String,
    pub sequence: i64,
    pub status: String,
    pub assigned_by: String,
    pub assigned_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub released_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = field_dispatch_assignments)]
pub struct NewFieldDispatchAssignment {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub technician_id: String,
    pub assigned_by: String,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub released_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = field_signoffs)]
pub struct FieldSignoff {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub customer_name: String,
    pub signature_ref: String,
    pub signed_at: DateTime<Utc>,
    pub notes: Option<String>,
    pub survey_distribution_id: Option<Uuid>,
    pub follow_up_status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = field_signoffs)]
pub struct NewFieldSignoff {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub customer_name: String,
    pub signature_ref: String,
    pub notes: Option<String>,
    pub survey_distribution_id: Option<Uuid>,
    pub created_by: String,
}

// ---------------------------------------------------------------------------
// Webinars (migration 000081_webinars).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = webinars)]
pub struct Webinar {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub host_id: String,
    pub co_host_ids: Vec<String>,
    pub panelist_ids: Vec<String>,
    pub status: String,
    pub access_model: String,
    pub product_id: Option<String>,
    pub capacity: i32,
    pub requires_approval: bool,
    pub allow_waitlist: bool,
    pub chat_enabled: bool,
    pub qa_enabled: bool,
    pub polls_enabled: bool,
    pub recording_enabled: bool,
    pub meeting_room_id: Option<Uuid>,
    pub meeting_link_status: String,
    pub chat_channel_id: Option<String>,
    pub survey_id: Option<String>,
    pub survey_distribution_id: Option<String>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub timezone: String,
    pub recurrence_rule: Option<String>,
    pub scheduler_job_id: Option<String>,
    pub scheduler_status: String,
    pub reminder_scheduler_status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub follow_up_sent_at: Option<DateTime<Utc>>,
    pub follow_up_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = webinars)]
pub struct NewWebinar {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub host_id: String,
    pub product_id: Option<String>,
    pub meeting_room_id: Option<Uuid>,
    pub chat_channel_id: Option<String>,
    pub survey_id: Option<String>,
    pub survey_distribution_id: Option<String>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub recurrence_rule: Option<String>,
    pub scheduler_job_id: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub follow_up_sent_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = webinar_registrations)]
pub struct WebinarRegistration {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub user_id: String,
    pub status: String,
    pub payment_status: String,
    pub registered_at: DateTime<Utc>,
    pub approved_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub promoted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = webinar_registrations)]
pub struct NewWebinarRegistration {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub user_id: String,
    pub approved_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub promoted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = webinar_polls)]
pub struct WebinarPoll {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub question: String,
    pub options: Vec<String>,
    pub allow_multiple: bool,
    pub status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = webinar_polls)]
pub struct NewWebinarPoll {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub question: String,
    pub options: Vec<String>,
    pub created_by: String,
    pub closed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = webinar_poll_votes)]
pub struct WebinarPollVote {
    pub id: Uuid,
    pub poll_id: Uuid,
    pub webinar_id: Uuid,
    pub user_id: String,
    pub option_indexes: Vec<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = webinar_poll_votes)]
pub struct NewWebinarPollVote {
    pub id: Uuid,
    pub poll_id: Uuid,
    pub webinar_id: Uuid,
    pub user_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = webinar_questions)]
pub struct WebinarQuestion {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub author_id: String,
    pub body: String,
    pub upvoted_by: Vec<String>,
    pub answered: bool,
    pub answered_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = webinar_questions)]
pub struct NewWebinarQuestion {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub author_id: String,
    pub body: String,
    pub answered_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = webinar_attendance)]
pub struct WebinarAttendance {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub user_id: String,
    pub joined_at: DateTime<Utc>,
    pub left_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = webinar_attendance)]
pub struct NewWebinarAttendance {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub user_id: String,
    pub left_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = webinar_recordings)]
pub struct WebinarRecording {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub meeting_recording_id: Option<Uuid>,
    pub title: Option<String>,
    pub status: String,
    pub storage_target: Option<String>,
    pub storage_key: Option<String>,
    pub storage_url: Option<String>,
    pub size_bytes: Option<i64>,
    pub duration_secs: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = webinar_recordings)]
pub struct NewWebinarRecording {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub meeting_recording_id: Option<Uuid>,
    pub title: Option<String>,
    pub storage_target: Option<String>,
    pub storage_key: Option<String>,
    pub storage_url: Option<String>,
    pub size_bytes: Option<i64>,
    pub duration_secs: Option<i64>,
}

// ---------------------------------------------------------------------------
// Cluster sync reliability (migration 000082_cluster_sync_reliability).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cluster_sync_conflicts)]
pub struct ClusterSyncConflict {
    pub id: Uuid,
    pub log_id: Option<Uuid>,
    pub plan_id: Option<Uuid>,
    pub cluster_id: Uuid,
    pub node_id: Option<Uuid>,
    pub domain: String,
    pub key: String,
    pub local_updated_at: Option<DateTime<Utc>>,
    pub remote_updated_at: Option<DateTime<Utc>>,
    pub winner: String,
    pub reason: String,
    pub local_payload: Option<Value>,
    pub remote_payload: Option<Value>,
    pub status: String,
    pub resolution: Option<String>,
    pub resolved_by: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cluster_sync_conflicts)]
pub struct NewClusterSyncConflict {
    pub id: Uuid,
    pub log_id: Option<Uuid>,
    pub plan_id: Option<Uuid>,
    pub cluster_id: Uuid,
    pub node_id: Option<Uuid>,
    pub domain: String,
    pub key: String,
    pub local_updated_at: Option<DateTime<Utc>>,
    pub remote_updated_at: Option<DateTime<Utc>>,
    pub winner: String,
    pub local_payload: Option<Value>,
    pub remote_payload: Option<Value>,
    pub resolution: Option<String>,
    pub resolved_by: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cluster_sync_retries)]
pub struct ClusterSyncRetry {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub cluster_id: Uuid,
    pub node_id: Option<Uuid>,
    pub domain: String,
    pub direction: String,
    pub attempt: i32,
    pub max_attempts: i32,
    pub next_retry_at: DateTime<Utc>,
    pub last_error: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cluster_sync_retries)]
pub struct NewClusterSyncRetry {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub cluster_id: Uuid,
    pub node_id: Option<Uuid>,
    pub domain: String,
    pub direction: String,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cluster_sync_idempotency_keys)]
pub struct ClusterSyncIdempotencyKey {
    pub scope: String,
    pub key: String,
    pub status: i32,
    pub body: Value,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cluster_sync_idempotency_keys)]
pub struct NewClusterSyncIdempotencyKey {
    pub scope: String,
    pub key: String,
    pub status: i32,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cluster_sync_settings)]
pub struct ClusterSyncSetting {
    pub key: String,
    pub value: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cluster_sync_settings)]
pub struct NewClusterSyncSetting {
    pub key: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = cluster_sync_domains)]
pub struct ClusterSyncDomain {
    pub domain: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = cluster_sync_domains)]
pub struct NewClusterSyncDomain {
    pub domain: String,
}

// ---------------------------------------------------------------------------
// Import / export hardening (migration 000083_import_export_hardening).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = mapping_templates)]
pub struct MappingTemplate {
    pub id: Uuid,
    pub name: String,
    pub entity: String,
    pub mapping: Value,
    pub actor: String,
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = mapping_templates)]
pub struct NewMappingTemplate {
    pub id: Uuid,
    pub name: String,
    pub entity: String,
    pub company_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = export_schedules)]
pub struct ExportSchedule {
    pub id: Uuid,
    pub name: String,
    pub entity: String,
    pub format: String,
    pub fields: Value,
    pub filters: Value,
    pub redact_pii: bool,
    pub redact_names: bool,
    pub pii_allowed: bool,
    pub schedule: String,
    pub enabled: bool,
    pub company_id: Option<Uuid>,
    pub actor: String,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_export_id: Option<Uuid>,
    pub last_error: Option<String>,
    pub run_count: i64,
    pub scheduling: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = export_schedules)]
pub struct NewExportSchedule {
    pub id: Uuid,
    pub name: String,
    pub entity: String,
    pub format: String,
    pub schedule: String,
    pub company_id: Option<Uuid>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_export_id: Option<Uuid>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = import_sessions)]
pub struct ImportSession {
    pub id: Uuid,
    pub entity: String,
    pub format: String,
    pub status: String,
    pub total_chunks: Option<i32>,
    pub received_chunks: Value,
    pub total_rows: i64,
    pub valid_rows: i64,
    pub invalid_rows: i64,
    pub row_errors: Value,
    pub mapping: Value,
    pub actor: String,
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = import_sessions)]
pub struct NewImportSession {
    pub id: Uuid,
    pub entity: String,
    pub format: String,
    pub status: String,
    pub total_chunks: Option<i32>,
    pub company_id: Option<Uuid>,
}

// ---------------------------------------------------------------------------
// Marketing automation (migration 000085_marketing).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketing_campaigns)]
pub struct MarketingCampaign {
    pub id: Uuid,
    pub name: String,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub template_id: Option<String>,
    pub segment_id: Option<Uuid>,
    pub newsletter_list_id: Option<String>,
    pub status: String,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub sent_at: Option<DateTime<Utc>>,
    pub ab_enabled: bool,
    pub ab_winner_metric: String,
    pub ab_min_sample: Option<i64>,
    pub winning_variant_id: Option<Uuid>,
    pub stats_queued: i64,
    pub stats_sent: i64,
    pub stats_delivered: i64,
    pub stats_failed: i64,
    pub stats_skipped: i64,
    pub stats_opened: i64,
    pub stats_clicked: i64,
    pub stats_unsubscribed: i64,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketing_campaigns)]
pub struct NewMarketingCampaign {
    pub id: Uuid,
    pub name: String,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub template_id: Option<String>,
    pub segment_id: Option<Uuid>,
    pub newsletter_list_id: Option<String>,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub sent_at: Option<DateTime<Utc>>,
    pub ab_min_sample: Option<i64>,
    pub winning_variant_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketing_campaign_variants)]
pub struct MarketingCampaignVariant {
    pub id: Uuid,
    pub campaign_id: Uuid,
    pub name: String,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub weight_percent: i16,
    pub sent: i64,
    pub opened: i64,
    pub clicked: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketing_campaign_variants)]
pub struct NewMarketingCampaignVariant {
    pub id: Uuid,
    pub campaign_id: Uuid,
    pub name: String,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketing_segments)]
pub struct MarketingSegment {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub rule_tree: Value,
    pub status: String,
    pub last_evaluated_at: Option<DateTime<Utc>>,
    pub last_size: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketing_segments)]
pub struct NewMarketingSegment {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub last_evaluated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketing_segment_size_history)]
pub struct MarketingSegmentSizeHistory {
    pub id: Uuid,
    pub segment_id: Uuid,
    pub size: i64,
    pub evaluated_contacts: i64,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketing_segment_size_history)]
pub struct NewMarketingSegmentSizeHistory {
    pub id: Uuid,
    pub segment_id: Uuid,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketing_sequences)]
pub struct MarketingSequence {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub trigger_kind: String,
    pub trigger_form_id: Option<String>,
    pub trigger_tag: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketing_sequences)]
pub struct NewMarketingSequence {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub trigger_form_id: Option<String>,
    pub trigger_tag: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketing_sequence_steps)]
pub struct MarketingSequenceStep {
    pub id: Uuid,
    pub sequence_id: Uuid,
    pub step_order: i64,
    pub delay_seconds: i64,
    pub condition: Value,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub template_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketing_sequence_steps)]
pub struct NewMarketingSequenceStep {
    pub id: Uuid,
    pub sequence_id: Uuid,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub template_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketing_enrollments)]
pub struct MarketingEnrollment {
    pub id: Uuid,
    pub sequence_id: Uuid,
    pub contact_id: String,
    pub email: String,
    pub next_step: i64,
    pub status: String,
    pub next_step_at: Option<DateTime<Utc>>,
    pub history: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketing_enrollments)]
pub struct NewMarketingEnrollment {
    pub id: Uuid,
    pub sequence_id: Uuid,
    pub contact_id: String,
    pub email: String,
    pub next_step_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketing_scores)]
pub struct MarketingScore {
    pub contact_id: String,
    pub email: String,
    pub score: i64,
    pub warm: bool,
    pub hot: bool,
    pub hot_notified_at: Option<DateTime<Utc>>,
    pub last_action_at: Option<DateTime<Utc>>,
    pub events: Value,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketing_scores)]
pub struct NewMarketingScore {
    pub contact_id: String,
    pub email: String,
    pub hot_notified_at: Option<DateTime<Utc>>,
    pub last_action_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketing_score_rules)]
pub struct MarketingScoreRule {
    pub id: Uuid,
    pub action: String,
    pub points: i64,
    pub decay_days: Option<i64>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketing_score_rules)]
pub struct NewMarketingScoreRule {
    pub id: Uuid,
    pub action: String,
    pub decay_days: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketing_sends)]
pub struct MarketingSend {
    pub id: Uuid,
    pub campaign_id: Option<Uuid>,
    pub sequence_id: Option<Uuid>,
    pub step_id: Option<Uuid>,
    pub enrollment_id: Option<Uuid>,
    pub contact_id: String,
    pub email: String,
    pub variant_id: Option<Uuid>,
    pub status: String,
    pub mail_job_id: Option<String>,
    pub skip_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketing_sends)]
pub struct NewMarketingSend {
    pub id: Uuid,
    pub campaign_id: Option<Uuid>,
    pub sequence_id: Option<Uuid>,
    pub step_id: Option<Uuid>,
    pub enrollment_id: Option<Uuid>,
    pub contact_id: String,
    pub email: String,
    pub variant_id: Option<Uuid>,
    pub mail_job_id: Option<String>,
    pub skip_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Manufacturing / MRP (migration 000086_manufacturing).
// Structs carrying NUMERIC columns skip serde derives (PgNumeric
// has no serde implementation), per the repo header rule.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = mfg_boms)]
pub struct MfgBom {
    pub id: Uuid,
    pub product_id: String,
    pub name: String,
    pub code: String,
    pub description: Option<String>,
    pub status: String,
    pub active_version_id: Option<Uuid>,
    pub latest_version: i32,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = mfg_boms)]
pub struct NewMfgBom {
    pub id: Uuid,
    pub product_id: String,
    pub name: String,
    pub code: String,
    pub description: Option<String>,
    pub active_version_id: Option<Uuid>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = mfg_bom_versions)]
pub struct MfgBomVersion {
    pub id: Uuid,
    pub bom_id: Uuid,
    pub version: i32,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = mfg_bom_versions)]
pub struct NewMfgBomVersion {
    pub id: Uuid,
    pub bom_id: Uuid,
    pub version: i32,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = mfg_bom_version_lines)]
pub struct MfgBomVersionLine {
    pub id: Uuid,
    pub bom_version_id: Uuid,
    pub component_product_id: String,
    pub quantity: PgNumeric,
    pub unit: String,
    pub scrap_allowance_percent: PgNumeric,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = mfg_bom_version_lines)]
pub struct NewMfgBomVersionLine {
    pub id: Uuid,
    pub bom_version_id: Uuid,
    pub component_product_id: String,
    pub quantity: PgNumeric,
    pub unit: String,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = mfg_work_centers)]
pub struct MfgWorkCenter {
    pub id: Uuid,
    pub name: String,
    pub code: String,
    pub capacity_hours_per_day: PgNumeric,
    pub cost_per_hour: Option<PgNumeric>,
    pub currency: Option<String>,
    pub status: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = mfg_work_centers)]
pub struct NewMfgWorkCenter {
    pub id: Uuid,
    pub name: String,
    pub code: String,
    pub cost_per_hour: Option<PgNumeric>,
    pub currency: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = mfg_bom_routing_steps)]
pub struct MfgBomRoutingStep {
    pub id: Uuid,
    pub bom_version_id: Uuid,
    pub sequence: i32,
    pub name: String,
    pub work_center_id: Uuid,
    pub duration_minutes: PgNumeric,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = mfg_bom_routing_steps)]
pub struct NewMfgBomRoutingStep {
    pub id: Uuid,
    pub bom_version_id: Uuid,
    pub sequence: i32,
    pub name: String,
    pub work_center_id: Uuid,
    pub duration_minutes: PgNumeric,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = mfg_work_orders)]
pub struct MfgWorkOrder {
    pub id: Uuid,
    pub number: String,
    pub bom_id: Uuid,
    pub bom_version_id: Uuid,
    pub bom_version: i32,
    pub product_id: String,
    pub quantity_planned: PgNumeric,
    pub quantity_produced: PgNumeric,
    pub quantity_scrapped: PgNumeric,
    pub status: String,
    pub planned_start: Option<DateTime<Utc>>,
    pub planned_end: Option<DateTime<Utc>>,
    pub actual_start: Option<DateTime<Utc>>,
    pub actual_end: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
    pub notes: Option<String>,
    pub inspection_status: Option<String>,
    pub inspection_notes: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = mfg_work_orders)]
pub struct NewMfgWorkOrder {
    pub id: Uuid,
    pub number: String,
    pub bom_id: Uuid,
    pub bom_version_id: Uuid,
    pub bom_version: i32,
    pub product_id: String,
    pub quantity_planned: PgNumeric,
    pub planned_start: Option<DateTime<Utc>>,
    pub planned_end: Option<DateTime<Utc>>,
    pub actual_start: Option<DateTime<Utc>>,
    pub actual_end: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
    pub notes: Option<String>,
    pub inspection_status: Option<String>,
    pub inspection_notes: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = mfg_work_order_requirements)]
pub struct MfgWorkOrderRequirement {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub component_product_id: String,
    pub quantity_required: PgNumeric,
    pub unit: String,
    pub quantity_reserved: PgNumeric,
    pub quantity_consumed: PgNumeric,
    pub quantity_short: PgNumeric,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = mfg_work_order_requirements)]
pub struct NewMfgWorkOrderRequirement {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub component_product_id: String,
    pub quantity_required: PgNumeric,
    pub unit: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = mfg_scrap_reasons)]
pub struct MfgScrapReason {
    pub id: Uuid,
    pub code: String,
    pub label: String,
    pub active: bool,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = mfg_scrap_reasons)]
pub struct NewMfgScrapReason {
    pub id: Uuid,
    pub code: String,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = mfg_units)]
pub struct MfgUnit {
    pub id: Uuid,
    pub code: String,
    pub label: String,
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = mfg_units)]
pub struct NewMfgUnit {
    pub id: Uuid,
    pub code: String,
    pub label: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = mfg_production_runs)]
pub struct MfgProductionRun {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub quantity_produced: PgNumeric,
    pub quantity_scrapped: PgNumeric,
    pub scrap_reason_id: Option<Uuid>,
    pub work_center_id: Option<Uuid>,
    pub operator_id: String,
    pub status: String,
    pub notes: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = mfg_production_runs)]
pub struct NewMfgProductionRun {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub quantity_produced: PgNumeric,
    pub scrap_reason_id: Option<Uuid>,
    pub work_center_id: Option<Uuid>,
    pub operator_id: String,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = mfg_reservations)]
pub struct MfgReservation {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub component_product_id: String,
    pub quantity_reserved: PgNumeric,
    pub quantity_consumed: PgNumeric,
    pub unit: String,
    pub status: String,
    pub stock_unknown: bool,
    pub confirmation_reference: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = mfg_reservations)]
pub struct NewMfgReservation {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub component_product_id: String,
    pub quantity_reserved: PgNumeric,
    pub unit: String,
    pub confirmation_reference: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = mfg_stock_notices)]
pub struct MfgStockNotice {
    pub id: Uuid,
    pub kind: String,
    pub work_order_id: Option<Uuid>,
    pub reservation_id: Option<Uuid>,
    pub component_product_id: Option<String>,
    pub quantity: Option<PgNumeric>,
    pub message: Option<String>,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = mfg_stock_notices)]
pub struct NewMfgStockNotice {
    pub id: Uuid,
    pub kind: String,
    pub work_order_id: Option<Uuid>,
    pub reservation_id: Option<Uuid>,
    pub component_product_id: Option<String>,
    pub quantity: Option<PgNumeric>,
    pub message: Option<String>,
}

// ---------------------------------------------------------------------------
// Quality management (migration 000087_quality).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = quality_points)]
pub struct QualityPoint {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub scope: String,
    pub target_ref: Option<String>,
    pub target_kind: Option<String>,
    pub check_kind: String,
    pub on_fail_policy: String,
    pub alert_severity: String,
    pub active: bool,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = quality_points)]
pub struct NewQualityPoint {
    pub id: Uuid,
    pub name: String,
    pub scope: String,
    pub target_ref: Option<String>,
    pub target_kind: Option<String>,
    pub check_kind: String,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = quality_criteria)]
pub struct QualityCriterion {
    pub id: Uuid,
    pub point_id: Uuid,
    pub name: String,
    pub kind: String,
    pub unit: Option<String>,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
    pub expected_bool: Option<bool>,
    pub expected_text: Option<String>,
    pub failure_cause: Option<String>,
    pub weight: f64,
    pub required: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = quality_criteria)]
pub struct NewQualityCriterion {
    pub id: Uuid,
    pub point_id: Uuid,
    pub name: String,
    pub kind: String,
    pub unit: Option<String>,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
    pub expected_bool: Option<bool>,
    pub expected_text: Option<String>,
    pub failure_cause: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = quality_inspections)]
pub struct QualityInspection {
    pub id: Uuid,
    pub point_id: Uuid,
    pub target_ref: String,
    pub target_kind: String,
    pub inspector_id: Option<String>,
    pub status: String,
    pub result: Option<String>,
    pub measurements: Value,
    pub failure_causes: Vec<String>,
    pub notes: Option<String>,
    pub evidence_refs: Vec<String>,
    pub inspected_at: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = quality_inspections)]
pub struct NewQualityInspection {
    pub id: Uuid,
    pub point_id: Uuid,
    pub target_ref: String,
    pub target_kind: String,
    pub inspector_id: Option<String>,
    pub status: String,
    pub result: Option<String>,
    pub notes: Option<String>,
    pub inspected_at: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = quality_alerts)]
pub struct QualityAlert {
    pub id: Uuid,
    pub inspection_id: Uuid,
    pub point_id: Uuid,
    pub target_ref: String,
    pub target_kind: String,
    pub severity: String,
    pub status: String,
    pub message: String,
    pub blocked: bool,
    pub acknowledged_by: Option<String>,
    pub resolved_by: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = quality_alerts)]
pub struct NewQualityAlert {
    pub id: Uuid,
    pub inspection_id: Uuid,
    pub point_id: Uuid,
    pub target_ref: String,
    pub target_kind: String,
    pub severity: String,
    pub status: String,
    pub message: String,
    pub acknowledged_by: Option<String>,
    pub resolved_by: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = quality_capas)]
pub struct QualityCapa {
    pub id: Uuid,
    pub inspection_id: Uuid,
    pub alert_id: Option<Uuid>,
    pub title: String,
    pub description: String,
    pub root_cause: Option<String>,
    pub owner_id: String,
    pub due_date: Option<DateTime<Utc>>,
    pub state: String,
    pub verification_notes: Option<String>,
    pub verified_by: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = quality_capas)]
pub struct NewQualityCapa {
    pub id: Uuid,
    pub inspection_id: Uuid,
    pub alert_id: Option<Uuid>,
    pub title: String,
    pub root_cause: Option<String>,
    pub owner_id: String,
    pub due_date: Option<DateTime<Utc>>,
    pub state: String,
    pub verification_notes: Option<String>,
    pub verified_by: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Expenses (migration 000088_expenses).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = expense_categories)]
pub struct ExpenseCategory {
    pub id: Uuid,
    pub name: String,
    pub code: String,
    pub company_id: Option<String>,
    pub receipt_required: bool,
    pub receipt_required_above_minor: Option<i64>,
    pub per_expense_limit_minor: Option<i64>,
    pub per_day_limit_minor: Option<i64>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = expense_categories)]
pub struct NewExpenseCategory {
    pub id: Uuid,
    pub name: String,
    pub code: String,
    pub company_id: Option<String>,
    pub receipt_required_above_minor: Option<i64>,
    pub per_expense_limit_minor: Option<i64>,
    pub per_day_limit_minor: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = expense_policies)]
pub struct ExpensePolicy {
    pub id: Uuid,
    pub name: String,
    pub company_id: Option<String>,
    pub currency: String,
    pub mileage_rate_minor: Option<i64>,
    pub mileage_unit: String,
    pub per_diem_rate_minor: Option<i64>,
    pub max_report_total_minor: Option<i64>,
    pub approval_chain_id: Option<String>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = expense_policies)]
pub struct NewExpensePolicy {
    pub id: Uuid,
    pub name: String,
    pub company_id: Option<String>,
    pub mileage_rate_minor: Option<i64>,
    pub per_diem_rate_minor: Option<i64>,
    pub max_report_total_minor: Option<i64>,
    pub approval_chain_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = expense_reports)]
pub struct ExpenseReport {
    pub id: Uuid,
    pub employee_id: String,
    pub company_id: Option<String>,
    pub policy_id: Option<Uuid>,
    pub title: String,
    pub notes: Option<String>,
    pub status: String,
    pub currency: String,
    pub total_minor: i64,
    pub violations: Vec<String>,
    pub approval_request_id: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decided_by: Option<String>,
    pub decision_reason: Option<String>,
    pub reimbursement_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = expense_reports)]
pub struct NewExpenseReport {
    pub id: Uuid,
    pub employee_id: String,
    pub company_id: Option<String>,
    pub policy_id: Option<Uuid>,
    pub title: String,
    pub notes: Option<String>,
    pub approval_request_id: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decided_by: Option<String>,
    pub decision_reason: Option<String>,
    pub reimbursement_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = expenses)]
pub struct Expense {
    pub id: Uuid,
    pub report_id: Uuid,
    pub kind: String,
    pub category_id: Uuid,
    pub expense_date: NaiveDate,
    pub amount_minor: i64,
    pub currency: String,
    pub description: String,
    pub project_id: Option<String>,
    pub client_id: Option<String>,
    pub distance: Option<f64>,
    pub days: Option<i64>,
    pub rate_minor_applied: Option<i64>,
    pub violations: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = expenses)]
pub struct NewExpense {
    pub id: Uuid,
    pub report_id: Uuid,
    pub kind: String,
    pub category_id: Uuid,
    pub expense_date: NaiveDate,
    pub amount_minor: i64,
    pub description: String,
    pub project_id: Option<String>,
    pub client_id: Option<String>,
    pub distance: Option<f64>,
    pub days: Option<i64>,
    pub rate_minor_applied: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = expense_receipts)]
pub struct ExpenseReceipt {
    pub id: Uuid,
    pub expense_id: Uuid,
    pub storage_target: String,
    pub storage_key: String,
    pub file_name: String,
    pub content_type: Option<String>,
    pub uploaded_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = expense_receipts)]
pub struct NewExpenseReceipt {
    pub id: Uuid,
    pub expense_id: Uuid,
    pub storage_target: String,
    pub storage_key: String,
    pub file_name: String,
    pub content_type: Option<String>,
    pub uploaded_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = expense_reimbursements)]
pub struct ExpenseReimbursement {
    pub id: Uuid,
    pub report_id: Uuid,
    pub employee_id: String,
    pub amount_minor: i64,
    pub currency: String,
    pub status: String,
    pub payable_reference: String,
    pub failure_reason: Option<String>,
    pub paid_at: Option<DateTime<Utc>>,
    pub attempts: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = expense_reimbursements)]
pub struct NewExpenseReimbursement {
    pub id: Uuid,
    pub report_id: Uuid,
    pub employee_id: String,
    pub amount_minor: i64,
    pub payable_reference: String,
    pub failure_reason: Option<String>,
    pub paid_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = expense_adjustments)]
pub struct ExpenseAdjustment {
    pub id: Uuid,
    pub report_id: Uuid,
    pub kind: String,
    pub amount_minor: i64,
    pub currency: String,
    pub reason: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = expense_adjustments)]
pub struct NewExpenseAdjustment {
    pub id: Uuid,
    pub report_id: Uuid,
    pub kind: String,
    pub amount_minor: i64,
    pub reason: String,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = expense_currency_rates)]
pub struct ExpenseCurrencyRate {
    pub id: Uuid,
    pub base_currency: String,
    pub target_currency: String,
    pub rate: f64,
    pub source: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = expense_currency_rates)]
pub struct NewExpenseCurrencyRate {
    pub id: Uuid,
    pub base_currency: String,
    pub target_currency: String,
    pub rate: f64,
}

// ---------------------------------------------------------------------------
// Approvals (migration 000089_approvals).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = approval_definitions)]
pub struct ApprovalDefinition {
    pub id: Uuid,
    pub module_: String,
    pub record_type: String,
    pub name: String,
    pub description: Option<String>,
    pub is_active: bool,
    pub timeout_hours: Option<i32>,
    pub escalation_action: String,
    pub escalate_to: Option<Value>,
    pub escalation_extension_hours: Option<i32>,
    pub company_id: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = approval_definitions)]
pub struct NewApprovalDefinition {
    pub id: Uuid,
    pub module_: String,
    pub record_type: String,
    pub name: String,
    pub description: Option<String>,
    pub timeout_hours: Option<i32>,
    pub escalate_to: Option<Value>,
    pub escalation_extension_hours: Option<i32>,
    pub company_id: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = approval_steps)]
pub struct ApprovalStep {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub position: i32,
    pub name: String,
    pub mode: String,
    pub approvers: Value,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = approval_steps)]
pub struct NewApprovalStep {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub position: i32,
    pub name: String,
    pub approvers: Value,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = approval_requests)]
pub struct ApprovalRequest {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub module_: String,
    pub record_type: String,
    pub record_id: String,
    pub requester_id: String,
    pub summary: String,
    pub amount_minor: Option<i64>,
    pub currency: Option<String>,
    pub status: String,
    pub current_step_index: i32,
    pub round: i32,
    pub escalation_count: i32,
    pub last_escalated_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decided_by: Option<String>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = approval_requests)]
pub struct NewApprovalRequest {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub module_: String,
    pub record_type: String,
    pub record_id: String,
    pub requester_id: String,
    pub summary: String,
    pub amount_minor: Option<i64>,
    pub currency: Option<String>,
    pub last_escalated_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decided_by: Option<String>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = approval_decisions)]
pub struct ApprovalDecision {
    pub id: Uuid,
    pub request_id: Uuid,
    pub step_index: i32,
    pub round: i32,
    pub approver_id: String,
    pub acting_for: Option<String>,
    pub slot_kind: String,
    pub slot_ref: String,
    pub decision: String,
    pub comment: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = approval_decisions)]
pub struct NewApprovalDecision {
    pub id: Uuid,
    pub request_id: Uuid,
    pub step_index: i32,
    pub round: i32,
    pub approver_id: String,
    pub acting_for: Option<String>,
    pub slot_kind: String,
    pub decision: String,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = approval_delegations)]
pub struct ApprovalDelegation {
    pub id: Uuid,
    pub delegator_id: String,
    pub delegate_id: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub is_active: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = approval_delegations)]
pub struct NewApprovalDelegation {
    pub id: Uuid,
    pub delegator_id: String,
    pub delegate_id: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = approval_escalations)]
pub struct ApprovalEscalation {
    pub id: Uuid,
    pub request_id: Uuid,
    pub step_index: i32,
    pub reason: String,
    pub action: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = approval_escalations)]
pub struct NewApprovalEscalation {
    pub id: Uuid,
    pub request_id: Uuid,
    pub step_index: i32,
    pub reason: String,
    pub action: String,
}

// ---------------------------------------------------------------------------
// Studio customizer (migration 000090_studio).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = studio_record_types)]
pub struct StudioRecordType {
    pub record_type: String,
    pub module_id: String,
    pub label: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = studio_record_types)]
pub struct NewStudioRecordType {
    pub record_type: String,
    pub module_id: String,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = studio_system_fields)]
pub struct StudioSystemField {
    pub id: Uuid,
    pub record_type: String,
    pub field_key: String,
    pub label: String,
    pub field_type: String,
    pub required: bool,
    pub hideable: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = studio_system_fields)]
pub struct NewStudioSystemField {
    pub id: Uuid,
    pub record_type: String,
    pub field_key: String,
    pub label: String,
    pub field_type: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = studio_field_definitions)]
pub struct StudioFieldDefinition {
    pub id: Uuid,
    pub company_id: String,
    pub record_type: String,
    pub field_key: String,
    pub label: String,
    pub field_type: String,
    pub version: i64,
    pub status: String,
    pub required: bool,
    pub default_value: Option<Value>,
    pub help_text: Option<String>,
    pub options: Value,
    pub validation: Value,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = studio_field_definitions)]
pub struct NewStudioFieldDefinition {
    pub id: Uuid,
    pub company_id: String,
    pub record_type: String,
    pub field_key: String,
    pub label: String,
    pub field_type: String,
    pub default_value: Option<Value>,
    pub help_text: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = studio_field_values)]
pub struct StudioFieldValue {
    pub id: Uuid,
    pub company_id: String,
    pub record_type: String,
    pub record_id: String,
    pub field_key: String,
    pub definition_id: Option<Uuid>,
    pub value_type: String,
    pub value: Value,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = studio_field_values)]
pub struct NewStudioFieldValue {
    pub id: Uuid,
    pub company_id: String,
    pub record_type: String,
    pub record_id: String,
    pub field_key: String,
    pub definition_id: Option<Uuid>,
    pub value_type: String,
    pub value: Value,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = studio_list_views)]
pub struct StudioListView {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: String,
    pub record_type: String,
    pub name: String,
    pub columns: Value,
    pub filters: Value,
    pub sort_field: Option<String>,
    pub sort_order: Option<String>,
    pub group_by: Option<String>,
    pub visibility: String,
    pub scope: String,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = studio_list_views)]
pub struct NewStudioListView {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: String,
    pub record_type: String,
    pub name: String,
    pub sort_field: Option<String>,
    pub sort_order: Option<String>,
    pub group_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = studio_form_layouts)]
pub struct StudioFormLayout {
    pub id: Uuid,
    pub company_id: String,
    pub record_type: String,
    pub name: String,
    pub sections: Value,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = studio_form_layouts)]
pub struct NewStudioFormLayout {
    pub id: Uuid,
    pub company_id: String,
    pub record_type: String,
    pub name: String,
}

// ---------------------------------------------------------------------------
// Printing (migration 000091_printing).
// PaperSize carries NUMERIC columns, so it skips serde derives
// (PgNumeric has no serde implementation).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = paper_sizes)]
pub struct PaperSize {
    pub code: String,
    pub name: String,
    pub width_mm: PgNumeric,
    pub height_mm: Option<PgNumeric>,
    pub margin_top_mm: PgNumeric,
    pub margin_bottom_mm: PgNumeric,
    pub margin_left_mm: PgNumeric,
    pub margin_right_mm: PgNumeric,
    pub is_standard: bool,
    pub is_thermal: bool,
    pub thermal_width_mm: Option<i32>,
    pub company_id: Option<String>,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = paper_sizes)]
pub struct NewPaperSize {
    pub code: String,
    pub name: String,
    pub width_mm: PgNumeric,
    pub height_mm: Option<PgNumeric>,
    pub is_standard: bool,
    pub is_thermal: bool,
    pub thermal_width_mm: Option<i32>,
    pub company_id: Option<String>,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = invoice_templates)]
pub struct InvoiceTemplate {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub name: String,
    pub is_default: bool,
    pub paper_size_code: Option<String>,
    pub header_text: Option<String>,
    pub footer_text: Option<String>,
    pub logo_storage_target: Option<String>,
    pub logo_storage_key: Option<String>,
    pub logo_url: Option<String>,
    pub primary_color: String,
    pub accent_color: String,
    pub font_family: String,
    pub columns: Value,
    pub line_item_layout: Value,
    pub show_tax_breakdown: bool,
    pub payment_instructions: Option<String>,
    pub footer_notes: Option<String>,
    pub version: i32,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = invoice_templates)]
pub struct NewInvoiceTemplate {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub name: String,
    pub paper_size_code: Option<String>,
    pub header_text: Option<String>,
    pub footer_text: Option<String>,
    pub logo_storage_target: Option<String>,
    pub logo_storage_key: Option<String>,
    pub logo_url: Option<String>,
    pub payment_instructions: Option<String>,
    pub footer_notes: Option<String>,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = invoice_template_versions)]
pub struct InvoiceTemplateVersion {
    pub id: Uuid,
    pub template_id: Uuid,
    pub version: i32,
    pub snapshot: Value,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = invoice_template_versions)]
pub struct NewInvoiceTemplateVersion {
    pub id: Uuid,
    pub template_id: Uuid,
    pub version: i32,
    pub snapshot: Value,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = printer_profiles)]
pub struct PrinterProfile {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub name: String,
    pub connection_type: String,
    pub connection_target: Option<String>,
    pub auth_token_ref: Option<String>,
    pub width_columns: i32,
    pub character_set: String,
    pub code_page: String,
    pub supports_drawer_kick: bool,
    pub supports_cut: bool,
    pub paper_size_code: Option<String>,
    pub is_default: bool,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = printer_profiles)]
pub struct NewPrinterProfile {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub name: String,
    pub connection_target: Option<String>,
    pub auth_token_ref: Option<String>,
    pub paper_size_code: Option<String>,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = print_jobs)]
pub struct PrintJob {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub invoice_id: String,
    pub template_id: Option<Uuid>,
    pub template_version: Option<i32>,
    pub paper_size_code: Option<String>,
    pub output_kind: String,
    pub printer_profile_id: Option<Uuid>,
    pub status: String,
    pub document_ref: Option<String>,
    pub escpos_preview: Option<String>,
    pub error_message: Option<String>,
    pub attempts: i32,
    pub requested_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = print_jobs)]
pub struct NewPrintJob {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub invoice_id: String,
    pub template_id: Option<Uuid>,
    pub template_version: Option<i32>,
    pub paper_size_code: Option<String>,
    pub printer_profile_id: Option<Uuid>,
    pub document_ref: Option<String>,
    pub escpos_preview: Option<String>,
    pub error_message: Option<String>,
    pub requested_by: String,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Curriculum (migration 000092_curriculum).
// Structs carrying NUMERIC columns skip serde derives (PgNumeric
// has no serde implementation), per the repo header rule.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = curriculum_frameworks)]
pub struct CurriculumFramework {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub description: String,
    pub country: String,
    pub is_active: bool,
    pub is_built_in: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = curriculum_frameworks)]
pub struct NewCurriculumFramework {
    pub id: Uuid,
    pub code: String,
    pub name: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = curriculum_levels)]
pub struct CurriculumLevel {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub code: String,
    pub name: String,
    pub grade_count: i32,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = curriculum_levels)]
pub struct NewCurriculumLevel {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub code: String,
    pub name: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = curriculum_subjects)]
pub struct CurriculumSubject {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub level_id: Uuid,
    pub code: String,
    pub name: String,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = curriculum_subjects)]
pub struct NewCurriculumSubject {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub level_id: Uuid,
    pub code: String,
    pub name: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = curriculum_core_competencies)]
pub struct CurriculumCoreCompetency {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub subject_id: Uuid,
    pub level_id: Uuid,
    pub code: String,
    pub title: String,
    pub description: String,
    pub grade_year: i32,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = curriculum_core_competencies)]
pub struct NewCurriculumCoreCompetency {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub subject_id: Uuid,
    pub level_id: Uuid,
    pub code: String,
    pub title: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = curriculum_basic_competencies)]
pub struct CurriculumBasicCompetency {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub core_competency_id: Uuid,
    pub code: String,
    pub title: String,
    pub description: String,
    pub grade_year: i32,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = curriculum_basic_competencies)]
pub struct NewCurriculumBasicCompetency {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub core_competency_id: Uuid,
    pub code: String,
    pub title: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = curriculum_structures)]
pub struct CurriculumStructure {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub level_id: Uuid,
    pub grade_year: i32,
    pub subject_id: Uuid,
    pub hours_per_week: PgNumeric,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = curriculum_structures)]
pub struct NewCurriculumStructure {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub level_id: Uuid,
    pub grade_year: i32,
    pub subject_id: Uuid,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = curriculum_assessment_domains)]
pub struct CurriculumAssessmentDomain {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub parent_domain_id: Option<Uuid>,
    pub description: String,
    pub sort_order: i32,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = curriculum_assessment_domains)]
pub struct NewCurriculumAssessmentDomain {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub parent_domain_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = curriculum_grade_descriptors)]
pub struct CurriculumGradeDescriptor {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub domain_id: Option<Uuid>,
    pub min_score: PgNumeric,
    pub max_score: PgNumeric,
    pub grade_label: String,
    pub descriptor: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = curriculum_grade_descriptors)]
pub struct NewCurriculumGradeDescriptor {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub domain_id: Option<Uuid>,
    pub min_score: PgNumeric,
    pub max_score: PgNumeric,
    pub grade_label: String,
    pub descriptor: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = curriculum_lesson_plans)]
pub struct CurriculumLessonPlan {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub subject_id: Uuid,
    pub core_competency_id: Option<Uuid>,
    pub basic_competency_id: Option<Uuid>,
    pub title: String,
    pub objectives: Vec<String>,
    pub materials: Vec<String>,
    pub activities: Vec<String>,
    pub assessment_notes: String,
    pub time_minutes: i32,
    pub grade_year: i32,
    pub semester: i32,
    pub teacher_id: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = curriculum_lesson_plans)]
pub struct NewCurriculumLessonPlan {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub subject_id: Uuid,
    pub core_competency_id: Option<Uuid>,
    pub basic_competency_id: Option<Uuid>,
    pub title: String,
    pub assessment_notes: String,
    pub teacher_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = curriculum_report_cards)]
pub struct CurriculumReportCard {
    pub id: Uuid,
    pub student_ref: String,
    pub framework_id: Uuid,
    pub level_id: Uuid,
    pub grade_year: i32,
    pub semester: i32,
    pub academic_year: String,
    pub status: String,
    pub issued_at: Option<DateTime<Utc>>,
    pub issued_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = curriculum_report_cards)]
pub struct NewCurriculumReportCard {
    pub id: Uuid,
    pub student_ref: String,
    pub framework_id: Uuid,
    pub level_id: Uuid,
    pub grade_year: i32,
    pub semester: i32,
    pub academic_year: String,
    pub issued_at: Option<DateTime<Utc>>,
    pub issued_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = curriculum_report_card_entries)]
pub struct CurriculumReportCardEntry {
    pub id: Uuid,
    pub report_card_id: Uuid,
    pub subject_id: Uuid,
    pub domain_id: Uuid,
    pub score: Option<PgNumeric>,
    pub grade_label: Option<String>,
    pub descriptor: Option<String>,
    pub notes: String,
    pub academic_grade_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = curriculum_report_card_entries)]
pub struct NewCurriculumReportCardEntry {
    pub id: Uuid,
    pub report_card_id: Uuid,
    pub subject_id: Uuid,
    pub domain_id: Uuid,
    pub score: Option<PgNumeric>,
    pub grade_label: Option<String>,
    pub descriptor: Option<String>,
    pub academic_grade_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = curriculum_interventions)]
pub struct CurriculumIntervention {
    pub id: Uuid,
    pub student_ref: String,
    pub basic_competency_id: Uuid,
    pub kind: String,
    pub status: String,
    pub teacher_id: String,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub notes: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = curriculum_interventions)]
pub struct NewCurriculumIntervention {
    pub id: Uuid,
    pub student_ref: String,
    pub basic_competency_id: Uuid,
    pub kind: String,
    pub teacher_id: String,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = curriculum_settings)]
pub struct CurriculumSetting {
    pub key: String,
    pub value: Value,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = curriculum_settings)]
pub struct NewCurriculumSetting {
    pub key: String,
    pub value: Value,
}

// ---------------------------------------------------------------------------
// Admissions (migration 000093_admissions).
// Structs carrying NUMERIC columns skip serde derives (PgNumeric
// has no serde implementation), per the repo header rule.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = admission_periods)]
pub struct AdmissionPeriod {
    pub id: Uuid,
    pub academic_year: String,
    pub term: String,
    pub name: String,
    pub open_date: NaiveDate,
    pub close_date: NaiveDate,
    pub status: String,
    pub registration_fee_amount: Option<PgNumeric>,
    pub fee_currency: String,
    pub registration_form_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = admission_periods)]
pub struct NewAdmissionPeriod {
    pub id: Uuid,
    pub academic_year: String,
    pub term: String,
    pub name: String,
    pub open_date: NaiveDate,
    pub close_date: NaiveDate,
    pub registration_fee_amount: Option<PgNumeric>,
    pub registration_form_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = admission_period_quotas)]
pub struct AdmissionPeriodQuota {
    pub id: Uuid,
    pub period_id: Uuid,
    pub program_id: String,
    pub level: String,
    pub seats: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = admission_period_quotas)]
pub struct NewAdmissionPeriodQuota {
    pub id: Uuid,
    pub period_id: Uuid,
    pub program_id: String,
    pub level: String,
    pub seats: i32,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = admission_stage_definitions)]
pub struct AdmissionStageDefinition {
    pub id: Uuid,
    pub period_id: Uuid,
    pub stage_key: String,
    pub name: String,
    pub position: i32,
    pub weight: PgNumeric,
    pub pass_score: Option<PgNumeric>,
    pub required: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = admission_stage_definitions)]
pub struct NewAdmissionStageDefinition {
    pub id: Uuid,
    pub period_id: Uuid,
    pub stage_key: String,
    pub name: String,
    pub pass_score: Option<PgNumeric>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = admission_required_documents)]
pub struct AdmissionRequiredDocument {
    pub id: Uuid,
    pub period_id: Uuid,
    pub document_key: String,
    pub title: String,
    pub description: Option<String>,
    pub required: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = admission_required_documents)]
pub struct NewAdmissionRequiredDocument {
    pub id: Uuid,
    pub period_id: Uuid,
    pub document_key: String,
    pub title: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = admission_applications)]
pub struct AdmissionApplication {
    pub id: Uuid,
    pub period_id: Uuid,
    pub applicant_user_id: String,
    pub full_name: String,
    pub email: String,
    pub phone: Option<String>,
    pub program_id: String,
    pub level: String,
    pub form_submission_id: Option<String>,
    pub status: String,
    pub total_score: Option<PgNumeric>,
    pub fee_amount: Option<PgNumeric>,
    pub fee_currency: String,
    pub fee_invoice_status: String,
    pub fee_invoice_id: Option<String>,
    pub student_id: Option<String>,
    pub source: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = admission_applications)]
pub struct NewAdmissionApplication {
    pub id: Uuid,
    pub period_id: Uuid,
    pub applicant_user_id: String,
    pub full_name: String,
    pub email: String,
    pub phone: Option<String>,
    pub program_id: String,
    pub level: String,
    pub form_submission_id: Option<String>,
    pub total_score: Option<PgNumeric>,
    pub fee_amount: Option<PgNumeric>,
    pub fee_invoice_id: Option<String>,
    pub student_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = admission_application_scores)]
pub struct AdmissionApplicationScore {
    pub id: Uuid,
    pub application_id: Uuid,
    pub period_id: Uuid,
    pub stage_id: Uuid,
    pub stage_key: String,
    pub score: PgNumeric,
    pub scored_by: String,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = admission_application_scores)]
pub struct NewAdmissionApplicationScore {
    pub id: Uuid,
    pub application_id: Uuid,
    pub period_id: Uuid,
    pub stage_id: Uuid,
    pub stage_key: String,
    pub score: PgNumeric,
    pub scored_by: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = admission_checklist_items)]
pub struct AdmissionChecklistItem {
    pub id: Uuid,
    pub application_id: Uuid,
    pub period_id: Uuid,
    pub required_document_id: Uuid,
    pub document_key: String,
    pub title: String,
    pub required: bool,
    pub status: String,
    pub document_id: Option<String>,
    pub verified_by: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
    pub rejection_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = admission_checklist_items)]
pub struct NewAdmissionChecklistItem {
    pub id: Uuid,
    pub application_id: Uuid,
    pub period_id: Uuid,
    pub required_document_id: Uuid,
    pub document_key: String,
    pub title: String,
    pub document_id: Option<String>,
    pub verified_by: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
    pub rejection_reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = admission_waitlist_entries)]
pub struct AdmissionWaitlistEntry {
    pub id: Uuid,
    pub period_id: Uuid,
    pub application_id: Uuid,
    pub position: i32,
    pub status: String,
    pub promoted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = admission_waitlist_entries)]
pub struct NewAdmissionWaitlistEntry {
    pub id: Uuid,
    pub period_id: Uuid,
    pub application_id: Uuid,
    pub position: i32,
    pub promoted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = admission_results)]
pub struct AdmissionResult {
    pub id: Uuid,
    pub period_id: Uuid,
    pub application_id: Uuid,
    pub decision: String,
    pub published: bool,
    pub published_at: Option<DateTime<Utc>>,
    pub decided_by: String,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = admission_results)]
pub struct NewAdmissionResult {
    pub id: Uuid,
    pub period_id: Uuid,
    pub application_id: Uuid,
    pub decision: String,
    pub published_at: Option<DateTime<Utc>>,
    pub decided_by: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = admission_settings)]
pub struct AdmissionSetting {
    pub key: String,
    pub value: String,
    pub description: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = admission_settings)]
pub struct NewAdmissionSetting {
    pub key: String,
    pub value: String,
}

// ---------------------------------------------------------------------------
// Academic system (migration 000094_academic).
// Structs carrying NUMERIC columns skip serde derives (PgNumeric
// has no serde implementation), per the repo header rule.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = academic_programs)]
pub struct AcademicProgram {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub department_id: Option<String>,
    pub max_credits_per_semester: Option<PgNumeric>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = academic_programs)]
pub struct NewAcademicProgram {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub department_id: Option<String>,
    pub max_credits_per_semester: Option<PgNumeric>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = academic_students)]
pub struct AcademicStudent {
    pub id: Uuid,
    pub user_id: String,
    pub student_number: String,
    pub program_id: Uuid,
    pub advisor_id: Option<Uuid>,
    pub enrollment_year: i32,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = academic_students)]
pub struct NewAcademicStudent {
    pub id: Uuid,
    pub user_id: String,
    pub student_number: String,
    pub program_id: Uuid,
    pub advisor_id: Option<Uuid>,
    pub enrollment_year: i32,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = academic_lecturers)]
pub struct AcademicLecturer {
    pub id: Uuid,
    pub user_id: String,
    pub lecturer_number: String,
    pub department_id: Option<String>,
    pub title: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = academic_lecturers)]
pub struct NewAcademicLecturer {
    pub id: Uuid,
    pub user_id: String,
    pub lecturer_number: String,
    pub department_id: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = academic_subjects)]
pub struct AcademicSubject {
    pub id: Uuid,
    pub code: String,
    pub title: String,
    pub program_id: Uuid,
    pub credits: PgNumeric,
    pub semester_number: Option<i32>,
    pub elearning_course_id: Option<String>,
    pub elearning_course_verified: bool,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = academic_subjects)]
pub struct NewAcademicSubject {
    pub id: Uuid,
    pub code: String,
    pub title: String,
    pub program_id: Uuid,
    pub credits: PgNumeric,
    pub semester_number: Option<i32>,
    pub elearning_course_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = academic_rooms)]
pub struct AcademicRoom {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub building: Option<String>,
    pub capacity: Option<i32>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = academic_rooms)]
pub struct NewAcademicRoom {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub building: Option<String>,
    pub capacity: Option<i32>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = academic_semesters)]
pub struct AcademicSemester {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = academic_semesters)]
pub struct NewAcademicSemester {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = academic_calendar_events)]
pub struct AcademicCalendarEvent {
    pub id: Uuid,
    pub semester_id: Option<Uuid>,
    pub event_type: String,
    pub title: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = academic_calendar_events)]
pub struct NewAcademicCalendarEvent {
    pub id: Uuid,
    pub semester_id: Option<Uuid>,
    pub event_type: String,
    pub title: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = academic_schedules)]
pub struct AcademicSchedule {
    pub id: Uuid,
    pub subject_id: Uuid,
    pub lecturer_id: Uuid,
    pub room_id: Uuid,
    pub semester_id: Uuid,
    pub day_of_week: i32,
    pub start_time: String,
    pub end_time: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = academic_schedules)]
pub struct NewAcademicSchedule {
    pub id: Uuid,
    pub subject_id: Uuid,
    pub lecturer_id: Uuid,
    pub room_id: Uuid,
    pub semester_id: Uuid,
    pub day_of_week: i32,
    pub start_time: String,
    pub end_time: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = academic_enrollments)]
pub struct AcademicEnrollment {
    pub id: Uuid,
    pub student_id: Uuid,
    pub subject_id: Uuid,
    pub semester_id: Uuid,
    pub credits: PgNumeric,
    pub status: String,
    pub advisor_id: Option<Uuid>,
    pub rejection_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = academic_enrollments)]
pub struct NewAcademicEnrollment {
    pub id: Uuid,
    pub student_id: Uuid,
    pub subject_id: Uuid,
    pub semester_id: Uuid,
    pub credits: PgNumeric,
    pub advisor_id: Option<Uuid>,
    pub rejection_reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = academic_attendance)]
pub struct AcademicAttendance {
    pub id: Uuid,
    pub schedule_id: Uuid,
    pub student_id: Uuid,
    pub date: NaiveDate,
    pub status: String,
    pub source: String,
    pub entry_id: Option<String>,
    pub minutes: Option<i64>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = academic_attendance)]
pub struct NewAcademicAttendance {
    pub id: Uuid,
    pub schedule_id: Uuid,
    pub student_id: Uuid,
    pub date: NaiveDate,
    pub entry_id: Option<String>,
    pub minutes: Option<i64>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = academic_grade_components)]
pub struct AcademicGradeComponent {
    pub id: Uuid,
    pub subject_id: Uuid,
    pub name: String,
    pub kind: String,
    pub weight_percent: PgNumeric,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = academic_grade_components)]
pub struct NewAcademicGradeComponent {
    pub id: Uuid,
    pub subject_id: Uuid,
    pub name: String,
    pub kind: String,
    pub weight_percent: PgNumeric,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = academic_grades)]
pub struct AcademicGrade {
    pub id: Uuid,
    pub enrollment_id: Uuid,
    pub component_id: Uuid,
    pub score: PgNumeric,
    pub graded_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = academic_grades)]
pub struct NewAcademicGrade {
    pub id: Uuid,
    pub enrollment_id: Uuid,
    pub component_id: Uuid,
    pub score: PgNumeric,
    pub graded_by: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = academic_grade_appeals)]
pub struct AcademicGradeAppeal {
    pub id: Uuid,
    pub enrollment_id: Uuid,
    pub component_id: Option<Uuid>,
    pub student_id: Uuid,
    pub reason: String,
    pub status: String,
    pub reviewed_by: Option<Uuid>,
    pub decision: Option<String>,
    pub corrected_score: Option<PgNumeric>,
    pub resolution_notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = academic_grade_appeals)]
pub struct NewAcademicGradeAppeal {
    pub id: Uuid,
    pub enrollment_id: Uuid,
    pub component_id: Option<Uuid>,
    pub student_id: Uuid,
    pub reason: String,
    pub reviewed_by: Option<Uuid>,
    pub decision: Option<String>,
    pub corrected_score: Option<PgNumeric>,
    pub resolution_notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = academic_transcripts)]
pub struct AcademicTranscript {
    pub id: Uuid,
    pub student_id: Uuid,
    pub issued_by: String,
    pub gpa: PgNumeric,
    pub total_credits: PgNumeric,
    pub lines: Value,
    pub print_job_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = academic_transcripts)]
pub struct NewAcademicTranscript {
    pub id: Uuid,
    pub student_id: Uuid,
    pub issued_by: String,
    pub gpa: PgNumeric,
    pub total_credits: PgNumeric,
    pub print_job_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = academic_theses)]
pub struct AcademicThesis {
    pub id: Uuid,
    pub student_id: Uuid,
    pub title: String,
    pub advisor_id: Uuid,
    pub co_advisor_id: Option<Uuid>,
    pub status: String,
    pub proposal_date: Option<NaiveDate>,
    pub defense_date: Option<NaiveDate>,
    pub defense_result: Option<String>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = academic_theses)]
pub struct NewAcademicThesis {
    pub id: Uuid,
    pub student_id: Uuid,
    pub title: String,
    pub advisor_id: Uuid,
    pub co_advisor_id: Option<Uuid>,
    pub proposal_date: Option<NaiveDate>,
    pub defense_date: Option<NaiveDate>,
    pub defense_result: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = academic_standing_records)]
pub struct AcademicStandingRecord {
    pub id: Uuid,
    pub student_id: Uuid,
    pub semester_id: Uuid,
    pub gpa: PgNumeric,
    pub total_credits: PgNumeric,
    pub standing: String,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = academic_standing_records)]
pub struct NewAcademicStandingRecord {
    pub id: Uuid,
    pub student_id: Uuid,
    pub semester_id: Uuid,
    pub gpa: PgNumeric,
    pub total_credits: PgNumeric,
    pub standing: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = academic_definitions)]
pub struct AcademicDefinition {
    pub id: Uuid,
    pub kind: String,
    pub key: String,
    pub label: String,
    pub config: Value,
    pub active: bool,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = academic_definitions)]
pub struct NewAcademicDefinition {
    pub id: Uuid,
    pub kind: String,
    pub key: String,
    pub label: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = academic_settings)]
pub struct AcademicSetting {
    pub key: String,
    pub value: Value,
    pub description: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = academic_settings)]
pub struct NewAcademicSetting {
    pub key: String,
    pub value: Value,
    pub description: Option<String>,
}

// ---------------------------------------------------------------------------
// Online exams (migration 000095_exams).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = exam_question_banks)]
pub struct ExamQuestionBank {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub category: String,
    pub owner_id: String,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = exam_question_banks)]
pub struct NewExamQuestionBank {
    pub id: Uuid,
    pub title: String,
    pub owner_id: String,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = exam_questions)]
pub struct ExamQuestion {
    pub id: Uuid,
    pub bank_id: Uuid,
    pub version: i32,
    pub previous_version_id: Option<Uuid>,
    pub superseded_by: Option<Uuid>,
    pub question_type: String,
    pub category: String,
    pub difficulty: String,
    pub text: String,
    pub options: Value,
    pub correct_answer: Option<Value>,
    pub answer_key: Option<String>,
    pub marks: f64,
    pub status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = exam_questions)]
pub struct NewExamQuestion {
    pub id: Uuid,
    pub bank_id: Uuid,
    pub previous_version_id: Option<Uuid>,
    pub superseded_by: Option<Uuid>,
    pub question_type: String,
    pub category: String,
    pub difficulty: String,
    pub text: String,
    pub correct_answer: Option<Value>,
    pub answer_key: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = exam_question_versions)]
pub struct ExamQuestionVersion {
    pub id: Uuid,
    pub question_id: Uuid,
    pub version: i32,
    pub snapshot: Value,
    pub reason: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = exam_question_versions)]
pub struct NewExamQuestionVersion {
    pub id: Uuid,
    pub question_id: Uuid,
    pub version: i32,
    pub reason: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = exams)]
pub struct Exam {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub category: String,
    pub bank_ids: Vec<Uuid>,
    pub course_id: Option<String>,
    pub academic_enrollment_id: Option<String>,
    pub academic_component_id: Option<String>,
    pub status: String,
    pub opens_at: Option<DateTime<Utc>>,
    pub closes_at: Option<DateTime<Utc>>,
    pub duration_minutes: i32,
    pub pass_mark: f64,
    pub attempts_allowed: i32,
    pub settings: Value,
    pub total_marks: f64,
    pub created_by: String,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = exams)]
pub struct NewExam {
    pub id: Uuid,
    pub title: String,
    pub course_id: Option<String>,
    pub academic_enrollment_id: Option<String>,
    pub academic_component_id: Option<String>,
    pub opens_at: Option<DateTime<Utc>>,
    pub closes_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = exam_question_links)]
pub struct ExamQuestionLink {
    pub id: Uuid,
    pub exam_id: Uuid,
    pub question_id: Uuid,
    pub question_version: i32,
    pub position: i32,
    pub section: Option<String>,
    pub marks: f64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = exam_question_links)]
pub struct NewExamQuestionLink {
    pub id: Uuid,
    pub exam_id: Uuid,
    pub question_id: Uuid,
    pub section: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = exam_rubrics)]
pub struct ExamRubric {
    pub id: Uuid,
    pub exam_id: Uuid,
    pub title: String,
    pub description: String,
    pub criteria: Value,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = exam_rubrics)]
pub struct NewExamRubric {
    pub id: Uuid,
    pub exam_id: Uuid,
    pub title: String,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = exam_attempts)]
pub struct ExamAttempt {
    pub id: Uuid,
    pub exam_id: Uuid,
    pub user_id: String,
    pub attempt_number: i32,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub duration_seconds: i64,
    pub seed: i64,
    pub question_order: Value,
    pub option_orders: Value,
    pub answers: Value,
    pub autosaved_at: Option<DateTime<Utc>>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub score: Option<f64>,
    pub max_score: f64,
    pub passed: Option<bool>,
    pub graded_at: Option<DateTime<Utc>>,
    pub result_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = exam_attempts)]
pub struct NewExamAttempt {
    pub id: Uuid,
    pub exam_id: Uuid,
    pub user_id: String,
    pub expires_at: DateTime<Utc>,
    pub duration_seconds: i64,
    pub seed: i64,
    pub autosaved_at: Option<DateTime<Utc>>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub score: Option<f64>,
    pub passed: Option<bool>,
    pub graded_at: Option<DateTime<Utc>>,
    pub result_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = exam_grades)]
pub struct ExamGrade {
    pub id: Uuid,
    pub attempt_id: Uuid,
    pub exam_id: Uuid,
    pub question_id: Uuid,
    pub grader_id: Option<String>,
    pub rubric_id: Option<Uuid>,
    pub criterion_scores: Value,
    pub marks_awarded: Option<f64>,
    pub max_marks: f64,
    pub status: String,
    pub comment: Option<String>,
    pub second_grader_id: Option<String>,
    pub audit: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = exam_grades)]
pub struct NewExamGrade {
    pub id: Uuid,
    pub attempt_id: Uuid,
    pub exam_id: Uuid,
    pub question_id: Uuid,
    pub grader_id: Option<String>,
    pub rubric_id: Option<Uuid>,
    pub marks_awarded: Option<f64>,
    pub comment: Option<String>,
    pub second_grader_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = exam_proctor_signals)]
pub struct ExamProctorSignal {
    pub id: Uuid,
    pub attempt_id: Uuid,
    pub exam_id: Uuid,
    pub user_id: String,
    pub signal_type: String,
    pub detail: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = exam_proctor_signals)]
pub struct NewExamProctorSignal {
    pub id: Uuid,
    pub attempt_id: Uuid,
    pub exam_id: Uuid,
    pub user_id: String,
    pub signal_type: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = exam_results)]
pub struct ExamResult {
    pub id: Uuid,
    pub attempt_id: Uuid,
    pub exam_id: Uuid,
    pub user_id: String,
    pub score: f64,
    pub max_score: f64,
    pub percentage: f64,
    pub passed: bool,
    pub status: String,
    pub per_question: Value,
    pub graded_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = exam_results)]
pub struct NewExamResult {
    pub id: Uuid,
    pub attempt_id: Uuid,
    pub exam_id: Uuid,
    pub user_id: String,
    pub published_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// School fees (migration 000096_school_fees).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = school_fee_types)]
pub struct SchoolFeeType {
    pub id: String,
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub category: String,
    pub billing_period: String,
    pub default_amount_minor: PgNumeric,
    pub currency: String,
    pub auto_assign_on_enrolment: bool,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = school_fee_types)]
pub struct NewSchoolFeeType {
    pub id: String,
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub category: String,
    pub billing_period: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = school_fee_schedules)]
pub struct SchoolFeeSchedule {
    pub id: String,
    pub student_user_id: String,
    pub program_code: String,
    pub level_code: String,
    pub academic_period_id: String,
    pub fee_type_id: String,
    pub amount_minor: PgNumeric,
    pub currency: String,
    pub due_date: DateTime<Utc>,
    pub status: String,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = school_fee_schedules)]
pub struct NewSchoolFeeSchedule {
    pub id: String,
    pub student_user_id: String,
    pub program_code: String,
    pub level_code: String,
    pub academic_period_id: String,
    pub fee_type_id: String,
    pub due_date: DateTime<Utc>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = school_fee_charges)]
pub struct SchoolFeeCharge {
    pub id: String,
    pub schedule_id: String,
    pub student_user_id: String,
    pub fee_type_id: String,
    pub academic_period_id: String,
    pub period_label: String,
    pub amount_minor: PgNumeric,
    pub discount_minor: PgNumeric,
    pub total_minor: PgNumeric,
    pub amount_paid_minor: PgNumeric,
    pub currency: String,
    pub invoice_id: Option<String>,
    pub discount_ids: Value,
    pub due_date: DateTime<Utc>,
    pub status: String,
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = school_fee_charges)]
pub struct NewSchoolFeeCharge {
    pub id: String,
    pub schedule_id: String,
    pub student_user_id: String,
    pub fee_type_id: String,
    pub academic_period_id: String,
    pub period_label: String,
    pub invoice_id: Option<String>,
    pub due_date: DateTime<Utc>,
    pub paid_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = school_fee_discounts)]
pub struct SchoolFeeDiscount {
    pub id: String,
    pub student_user_id: String,
    pub fee_type_id: Option<String>,
    pub kind: String,
    pub percent_basis_points: i64,
    pub amount_minor: PgNumeric,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    pub approved: bool,
    pub approved_by: Option<String>,
    pub reason: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = school_fee_discounts)]
pub struct NewSchoolFeeDiscount {
    pub id: String,
    pub student_user_id: String,
    pub fee_type_id: Option<String>,
    pub kind: String,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    pub approved_by: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = school_fee_receipts)]
pub struct SchoolFeeReceipt {
    pub id: String,
    pub number: String,
    pub charge_id: String,
    pub student_user_id: String,
    pub amount_minor: PgNumeric,
    pub currency: String,
    pub method: Option<String>,
    pub reference: Option<String>,
    pub issued_by: String,
    pub issued_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = school_fee_receipts)]
pub struct NewSchoolFeeReceipt {
    pub id: String,
    pub number: String,
    pub charge_id: String,
    pub student_user_id: String,
    pub method: Option<String>,
    pub reference: Option<String>,
    pub issued_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = school_fee_guardians)]
pub struct SchoolFeeGuardian {
    pub id: String,
    pub guardian_user_id: String,
    pub student_user_id: String,
    pub relationship: String,
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = school_fee_guardians)]
pub struct NewSchoolFeeGuardian {
    pub id: String,
    pub guardian_user_id: String,
    pub student_user_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = school_fee_reminders)]
pub struct SchoolFeeReminder {
    pub charge_id: String,
    pub offset_days: i32,
    pub recipient_user_id: String,
    pub sent_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = school_fee_reminders)]
pub struct NewSchoolFeeReminder {
    pub charge_id: String,
    pub offset_days: i32,
    pub recipient_user_id: String,
}

// ---------------------------------------------------------------------------
// Library (migration 000097_library).
// Structs carrying NUMERIC columns skip serde derives (PgNumeric
// has no serde implementation), per the repo header rule.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = library_categories)]
pub struct LibraryCategory {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub parent_id: Option<Uuid>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = library_categories)]
pub struct NewLibraryCategory {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub parent_id: Option<Uuid>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = library_items)]
pub struct LibraryItem {
    pub id: Uuid,
    pub title: String,
    pub subtitle: Option<String>,
    pub authors: Value,
    pub isbn: Option<String>,
    pub isbn_normalized: Option<String>,
    pub edition: Option<String>,
    pub series: Option<String>,
    pub series_position: Option<i64>,
    pub publisher: Option<String>,
    pub published_year: Option<i64>,
    pub language: String,
    pub category_id: Option<Uuid>,
    pub category_name: Option<String>,
    pub item_type: String,
    pub status: String,
    pub description: Option<String>,
    pub cover_ref: Option<String>,
    pub digital_ref: Option<String>,
    pub document_ref: Option<String>,
    pub storage_ref: Option<String>,
    pub tags: Value,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = library_items)]
pub struct NewLibraryItem {
    pub id: Uuid,
    pub title: String,
    pub subtitle: Option<String>,
    pub isbn: Option<String>,
    pub isbn_normalized: Option<String>,
    pub edition: Option<String>,
    pub series: Option<String>,
    pub series_position: Option<i64>,
    pub publisher: Option<String>,
    pub published_year: Option<i64>,
    pub language: String,
    pub category_id: Option<Uuid>,
    pub category_name: Option<String>,
    pub item_type: String,
    pub status: String,
    pub description: Option<String>,
    pub cover_ref: Option<String>,
    pub digital_ref: Option<String>,
    pub document_ref: Option<String>,
    pub storage_ref: Option<String>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = library_copies)]
pub struct LibraryCopy {
    pub id: Uuid,
    pub item_id: Uuid,
    pub barcode: String,
    pub serial: Option<String>,
    pub condition: String,
    pub location: Option<String>,
    pub shelf: Option<String>,
    pub status: String,
    pub notes: Option<String>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = library_copies)]
pub struct NewLibraryCopy {
    pub id: Uuid,
    pub item_id: Uuid,
    pub barcode: String,
    pub serial: Option<String>,
    pub condition: String,
    pub location: Option<String>,
    pub shelf: Option<String>,
    pub status: String,
    pub notes: Option<String>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = library_members)]
pub struct LibraryMember {
    pub id: Uuid,
    pub user_id: String,
    pub member_type: String,
    pub status: String,
    pub joined_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = library_members)]
pub struct NewLibraryMember {
    pub id: Uuid,
    pub user_id: String,
    pub member_type: String,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = library_loans)]
pub struct LibraryLoan {
    pub id: Uuid,
    pub member_id: Uuid,
    pub user_id: String,
    pub item_id: Uuid,
    pub copy_id: Option<Uuid>,
    pub status: String,
    pub checked_out_at: DateTime<Utc>,
    pub due_at: DateTime<Utc>,
    pub returned_at: Option<DateTime<Utc>>,
    pub renewal_count: i64,
    pub last_reminded_at: Option<DateTime<Utc>>,
    pub last_overdue_noticed_at: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = library_loans)]
pub struct NewLibraryLoan {
    pub id: Uuid,
    pub member_id: Uuid,
    pub user_id: String,
    pub item_id: Uuid,
    pub copy_id: Option<Uuid>,
    pub status: String,
    pub due_at: DateTime<Utc>,
    pub returned_at: Option<DateTime<Utc>>,
    pub last_reminded_at: Option<DateTime<Utc>>,
    pub last_overdue_noticed_at: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = library_holds)]
pub struct LibraryHold {
    pub id: Uuid,
    pub member_id: Uuid,
    pub user_id: String,
    pub item_id: Uuid,
    pub copy_id: Option<Uuid>,
    pub status: String,
    pub position: i64,
    pub ready_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = library_holds)]
pub struct NewLibraryHold {
    pub id: Uuid,
    pub member_id: Uuid,
    pub user_id: String,
    pub item_id: Uuid,
    pub copy_id: Option<Uuid>,
    pub status: String,
    pub position: i64,
    pub ready_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = library_fines)]
pub struct LibraryFine {
    pub id: Uuid,
    pub loan_id: Uuid,
    pub member_id: Uuid,
    pub user_id: String,
    pub amount_minor: PgNumeric,
    pub currency: String,
    pub reason: String,
    pub status: String,
    pub waived_by: Option<String>,
    pub paid_reference: Option<String>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = library_fines)]
pub struct NewLibraryFine {
    pub id: Uuid,
    pub loan_id: Uuid,
    pub member_id: Uuid,
    pub user_id: String,
    pub amount_minor: PgNumeric,
    pub currency: String,
    pub reason: String,
    pub status: String,
    pub waived_by: Option<String>,
    pub paid_reference: Option<String>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = library_loan_policies)]
pub struct LibraryLoanPolicy {
    pub id: Uuid,
    pub name: String,
    pub member_type: String,
    pub item_type: String,
    pub loan_days: i64,
    pub max_concurrent: i64,
    pub max_renewals: i64,
    pub fine_per_day_minor: PgNumeric,
    pub currency: String,
    pub grace_days: i64,
    pub active: bool,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = library_loan_policies)]
pub struct NewLibraryLoanPolicy {
    pub id: Uuid,
    pub name: String,
    pub member_type: String,
    pub item_type: String,
    pub loan_days: i64,
    pub max_concurrent: i64,
    pub max_renewals: i64,
    pub fine_per_day_minor: PgNumeric,
    pub currency: String,
    pub grace_days: i64,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = library_inventory_audits)]
pub struct LibraryInventoryAudit {
    pub id: Uuid,
    pub name: String,
    pub scope_location: Option<String>,
    pub scope_shelf: Option<String>,
    pub status: String,
    pub started_by: String,
    pub completed_at: Option<DateTime<Utc>>,
    pub items_checked: i64,
    pub discrepancies_found: i64,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = library_inventory_audits)]
pub struct NewLibraryInventoryAudit {
    pub id: Uuid,
    pub name: String,
    pub scope_location: Option<String>,
    pub scope_shelf: Option<String>,
    pub status: String,
    pub started_by: String,
    pub completed_at: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = library_inventory_discrepancies)]
pub struct LibraryInventoryDiscrepancy {
    pub id: Uuid,
    pub audit_id: Uuid,
    pub copy_id: Uuid,
    pub item_id: Uuid,
    pub kind: String,
    pub note: Option<String>,
    pub status: String,
    pub resolved_by: Option<String>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = library_inventory_discrepancies)]
pub struct NewLibraryInventoryDiscrepancy {
    pub id: Uuid,
    pub audit_id: Uuid,
    pub copy_id: Uuid,
    pub item_id: Uuid,
    pub kind: String,
    pub note: Option<String>,
    pub resolved_by: Option<String>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = library_digital_access)]
pub struct LibraryDigitalAccess {
    pub id: Uuid,
    pub loan_id: Uuid,
    pub item_id: Uuid,
    pub member_id: Uuid,
    pub user_id: String,
    pub expires_at: DateTime<Utc>,
    pub revoked: bool,
    pub redeemed_at: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = library_digital_access)]
pub struct NewLibraryDigitalAccess {
    pub id: Uuid,
    pub loan_id: Uuid,
    pub item_id: Uuid,
    pub member_id: Uuid,
    pub user_id: String,
    pub expires_at: DateTime<Utc>,
    pub redeemed_at: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Calendar (migration 000101_calendar). `module` is a Rust keyword,
// so the struct field is `module_` (Diesel strips the underscore).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = calendar_feeds)]
pub struct CalendarFeed {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: Option<String>,
    pub user_id: String,
    pub token_hash: String,
    pub modules: Value,
    pub label: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub revoked: bool,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = calendar_feeds)]
pub struct NewCalendarFeed {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: Option<String>,
    pub user_id: String,
    pub token_hash: String,
    pub label: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = calendar_settings)]
pub struct CalendarSetting {
    pub id: Uuid,
    pub tenant_id: String,
    pub scope: String,
    pub owner_id: String,
    pub weekend_days: Value,
    pub working_hours_start: String,
    pub working_hours_end: String,
    pub default_view: String,
    pub timezone: String,
    pub holiday_country: String,
    pub create_link_templates: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = calendar_settings)]
pub struct NewCalendarSetting {
    pub id: Uuid,
    pub tenant_id: String,
    pub scope: String,
    pub owner_id: String,
    pub holiday_country: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = calendar_sources)]
pub struct CalendarSource {
    pub id: Uuid,
    pub tenant_id: String,
    pub user_id: String,
    pub module_: String,
    pub visible: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = calendar_sources)]
pub struct NewCalendarSource {
    pub id: Uuid,
    pub tenant_id: String,
    pub user_id: String,
    pub module_: String,
}

// ---------------------------------------------------------------------------
// SaaS packages (migration 000102_saas_packages).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = saas_packages)]
pub struct SaasPackage {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub status: String,
    pub current_version: i32,
    pub product_id: Option<String>,
    pub preset_settings: Value,
    pub wizard_steps: Value,
    pub created_by: String,
    pub vendor_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = saas_packages)]
pub struct NewSaasPackage {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub status: String,
    pub product_id: Option<String>,
    pub created_by: String,
    pub vendor_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = saas_package_modules)]
pub struct SaasPackageModule {
    pub package_id: String,
    pub version: i32,
    pub module_id: String,
    pub required: bool,
    pub sort_order: i32,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = saas_package_modules)]
pub struct NewSaasPackageModule {
    pub package_id: String,
    pub version: i32,
    pub module_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = saas_package_purchases)]
pub struct SaasPackagePurchase {
    pub id: String,
    pub package_id: String,
    pub package_version: i32,
    pub buyer_id: String,
    pub company_id: String,
    pub status: String,
    pub subdomain: Option<String>,
    pub subdomain_pending: bool,
    pub order_id: Option<String>,
    pub subscription_id: Option<String>,
    pub product_id: Option<String>,
    pub enabled_modules: Value,
    pub grace_ends_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
    pub provision_attempts: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = saas_package_purchases)]
pub struct NewSaasPackagePurchase {
    pub id: String,
    pub package_id: String,
    pub package_version: i32,
    pub buyer_id: String,
    pub company_id: String,
    pub status: String,
    pub subdomain: Option<String>,
    pub order_id: Option<String>,
    pub subscription_id: Option<String>,
    pub product_id: Option<String>,
    pub grace_ends_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = saas_package_provision_steps)]
pub struct SaasPackageProvisionStep {
    pub id: String,
    pub purchase_id: String,
    pub step: String,
    pub status: String,
    pub outcome: Option<String>,
    pub detail: Option<String>,
    pub attempt: i32,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = saas_package_provision_steps)]
pub struct NewSaasPackageProvisionStep {
    pub id: String,
    pub purchase_id: String,
    pub step: String,
    pub status: String,
    pub outcome: Option<String>,
    pub detail: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Holidays (migration 000104_holidays).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = holiday_providers)]
pub struct HolidayProvider {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub allowed_hosts: Value,
    pub api_key_required: bool,
    #[serde(skip_serializing)]
    pub api_key_encrypted: Option<String>,
    pub key_fingerprint: Option<String>,
    pub min_interval_secs: i64,
    pub max_requests_per_minute: i64,
    pub calendar_ids: Value,
    pub enabled: bool,
    pub builtin: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = holiday_providers)]
pub struct NewHolidayProvider {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub api_key_encrypted: Option<String>,
    pub key_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = holiday_countries)]
pub struct HolidayCountry {
    pub code: String,
    pub name: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = holiday_countries)]
pub struct NewHolidayCountry {
    pub code: String,
    pub name: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = holiday_sync_configs)]
pub struct HolidaySyncConfig {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub provider_id: String,
    pub fallback_provider_ids: Value,
    pub countries: Value,
    pub years: Value,
    pub schedule: String,
    pub feed_url: Option<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = holiday_sync_configs)]
pub struct NewHolidaySyncConfig {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub provider_id: String,
    pub schedule: String,
    pub feed_url: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = holiday_sync_runs)]
pub struct HolidaySyncRun {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub config_id: Uuid,
    pub provider_id: String,
    pub country: String,
    pub year: i64,
    pub status: String,
    pub trigger: String,
    pub holidays_found: i64,
    pub holidays_stored: i64,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub fetched_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = holiday_sync_runs)]
pub struct NewHolidaySyncRun {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub config_id: Uuid,
    pub provider_id: String,
    pub country: String,
    pub year: i64,
    pub status: String,
    pub trigger: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = holidays)]
pub struct Holiday {
    pub id: Uuid,
    pub country: String,
    pub date: NaiveDate,
    pub name: String,
    pub local_name: Option<String>,
    pub provider_id: String,
    pub source: String,
    pub global: bool,
    pub types: Value,
    pub counties: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = holidays)]
pub struct NewHoliday {
    pub id: Uuid,
    pub country: String,
    pub date: NaiveDate,
    pub name: String,
    pub local_name: Option<String>,
    pub provider_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = holiday_overrides)]
pub struct HolidayOverride {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub country: String,
    pub date: NaiveDate,
    pub name: String,
    pub override_kind: String,
    pub note: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = holiday_overrides)]
pub struct NewHolidayOverride {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub country: String,
    pub date: NaiveDate,
    pub name: String,
    pub override_kind: String,
    pub note: Option<String>,
    pub created_by: String,
}

// ---------------------------------------------------------------------------
// Tenant destroy requests (no migration yet; derived from the
// `DestroyRequest` struct in src/modules/tenant_destroy.rs).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = tenant_destroy_requests)]
pub struct TenantDestroyRequest {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub tenant_slug: String,
    pub status: String,
    pub requested_by: String,
    pub requested_at: DateTime<Utc>,
    pub reason: Option<String>,
    pub export_job_id: Option<Uuid>,
    pub export_requested_at: Option<DateTime<Utc>>,
    pub export_completed_at: Option<DateTime<Utc>>,
    pub export_download_url: Option<String>,
    pub export_verified_via: Option<String>,
    pub export_request_claimed: bool,
    pub confirmation_attempts: i32,
    pub failed_confirmation_attempts: i32,
    pub confirmation_locked_until: Option<DateTime<Utc>>,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub confirmed_by: Option<String>,
    pub second_approval_required: bool,
    pub second_approval_reason: Option<String>,
    pub second_approver: Option<String>,
    pub second_approved_at: Option<DateTime<Utc>>,
    pub grace_period_days: i64,
    pub grace_expires_at: Option<DateTime<Utc>>,
    pub suspended_at: Option<DateTime<Utc>>,
    pub destroyed_at: Option<DateTime<Utc>>,
    pub schema_name_snapshot: Option<String>,
    pub subdomains_revoke_requested: bool,
    pub external_cleanup: Value,
    pub audit_trail: Value,
    pub cancel_reason: Option<String>,
    pub cancelled_by: Option<String>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = tenant_destroy_requests)]
pub struct NewTenantDestroyRequest {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub tenant_slug: String,
    pub status: String,
    pub requested_by: String,
    pub requested_at: DateTime<Utc>,
    pub reason: Option<String>,
    pub export_job_id: Option<Uuid>,
    pub export_requested_at: Option<DateTime<Utc>>,
    pub export_completed_at: Option<DateTime<Utc>>,
    pub export_download_url: Option<String>,
    pub export_verified_via: Option<String>,
    pub export_request_claimed: bool,
    pub confirmation_attempts: i32,
    pub failed_confirmation_attempts: i32,
    pub confirmation_locked_until: Option<DateTime<Utc>>,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub confirmed_by: Option<String>,
    pub second_approval_required: bool,
    pub second_approval_reason: Option<String>,
    pub second_approver: Option<String>,
    pub second_approved_at: Option<DateTime<Utc>>,
    pub grace_period_days: i64,
    pub grace_expires_at: Option<DateTime<Utc>>,
    pub suspended_at: Option<DateTime<Utc>>,
    pub destroyed_at: Option<DateTime<Utc>>,
    pub schema_name_snapshot: Option<String>,
    pub subdomains_revoke_requested: bool,
    pub external_cleanup: Value,
    pub audit_trail: Value,
    pub cancel_reason: Option<String>,
    pub cancelled_by: Option<String>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}
