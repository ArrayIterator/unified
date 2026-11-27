// Diesel models for the pricing domain of the unified-db-commerce bounded context.
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

// Pricing engine (batch 15 B3 notes sec. 6.3). Money structs carry
// `PgNumeric` fields, so per the repo header rule they skip ALL
// serde derives (Queryable: no Serialize; New*: no Deserialize).
// `PricingTier` / `NewPricingTier` already exist for the 000041
// `pricing_tiers` shape; the 000103 volume tiers are
// `PricingVolumeTier` / `NewPricingVolumeTier` below (the 000103
// table was renamed to `pricing_volume_tiers` during the final
// inspection to resolve the name collision).

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
#[diesel(table_name = pricing_volume_tiers)]
pub struct PricingVolumeTier {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub product_id: String,
    pub min_quantity: i64,
    pub max_quantity: Option<i64>,
    pub unit_price_scaled: PgNumeric,
    pub currency: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pricing_volume_tiers)]
pub struct NewPricingVolumeTier {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub product_id: String,
    pub min_quantity: i64,
    pub max_quantity: Option<i64>,
    pub unit_price_scaled: PgNumeric,
    pub currency: String,
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
