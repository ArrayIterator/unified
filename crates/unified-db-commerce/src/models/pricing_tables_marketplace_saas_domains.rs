// Diesel models for the pricing_tables_marketplace_saas_domains domain of the unified-db-commerce bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
