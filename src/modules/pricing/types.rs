// pricing - types.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::middleware::TenantContext;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::money::*;
use super::recommendations::*;
use super::discounts::*;
use super::proration::*;
use super::reconciliation::*;
use super::store::*;
use super::common::*;
use super::service::*;
use super::handlers_recommendations::*;
use super::handlers_rules::*;
use super::handlers_coupons::*;
use super::handlers_prices::*;
use super::handlers_bundles::*;
use super::handlers_reconciliation::*;
use super::module::*;

// Stored types.

/// Admin-editable module settings. Every value is a configurable
/// default: the code never hard-codes a threshold, a basis, or a
/// currency beyond the documented defaults below.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingSettings {
    /// Rounding rule. Only `half_up` is implemented.
    pub rounding_mode: String,
    /// `per_day` or `per_month`.
    pub proration_basis: String,
    /// Days in one month when the proration basis is `per_month`.
    pub days_per_month: i64,
    /// Price change above this many basis points needs a second
    /// approver.
    pub approval_threshold_bp: i64,
    /// Whether a second approver is required at all; when false,
    /// a price version above the threshold still records the
    /// threshold breach in its audit trail but may be approved by
    /// the requester.
    pub require_second_approver: bool,
    pub default_currency: String,
}

impl Default for PricingSettings {
    fn default() -> Self {
        Self {
            rounding_mode: "half_up".to_string(),
            proration_basis: "per_day".to_string(),
            days_per_month: DEFAULT_DAYS_PER_MONTH,
            approval_threshold_bp: DEFAULT_APPROVAL_THRESHOLD_BP,
            require_second_approver: true,
            default_currency: DEFAULT_CURRENCY.to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePricingSettings {
    #[serde(default)]
    pub rounding_mode: Option<String>,
    #[serde(default)]
    pub proration_basis: Option<String>,
    #[serde(default)]
    pub days_per_month: Option<i64>,
    #[serde(default)]
    pub approval_threshold_bp: Option<i64>,
    #[serde(default)]
    pub require_second_approver: Option<bool>,
    #[serde(default)]
    pub default_currency: Option<String>,
}

/// One currency the platform prices in. ISO 4217 codes are the
/// standard constants; the rows, the exponents, and the exchange
/// rate ratios are data. The rate is relative to the module
/// default currency and is entered by an admin only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingCurrency {
    pub code: String,
    /// Fractional digits in everyday prices: 0 or 2.
    pub exponent: i64,
    pub active: bool,
    /// Exchange-rate numerator relative to the default currency.
    #[serde(default)]
    pub rate_numerator: Option<i64>,
    /// Exchange-rate denominator relative to the default currency.
    #[serde(default)]
    pub rate_denominator: Option<i64>,
    #[serde(default)]
    pub rate_fetched_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePricingCurrency {
    pub code: String,
    #[serde(default)]
    pub exponent: Option<i64>,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub rate_numerator: Option<i64>,
    #[serde(default)]
    pub rate_denominator: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePricingCurrency {
    #[serde(default)]
    pub exponent: Option<i64>,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub rate_numerator: Option<i64>,
    #[serde(default)]
    pub rate_denominator: Option<i64>,
}

/// One discount rule. Rules are data: kind, order, scope, and
/// limits are admin-editable, never baked into code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingRule {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub name: String,
    /// `percent`, `fixed`, `buy_x_get_y`, or `volume`.
    pub kind: String,
    #[serde(default)]
    pub percent_bp: i64,
    /// Fixed amount in scaled minor units (kind `fixed` only).
    #[serde(default)]
    pub amount_scaled: i64,
    pub currency: String,
    #[serde(default)]
    pub buy_quantity: u32,
    #[serde(default)]
    pub get_quantity: u32,
    #[serde(default)]
    pub min_quantity: u32,
    /// Minimum order subtotal (scaled) for the rule to apply.
    #[serde(default)]
    pub min_subtotal_scaled: i64,
    /// Empty means the rule applies to every product.
    #[serde(default)]
    pub product_ids: Vec<String>,
    /// Declared evaluation order; lower runs first.
    pub evaluation_order: i64,
    pub stackable: bool,
    pub exclusive: bool,
    /// `active`, `draft`, or `archived`.
    pub status: String,
    #[serde(default)]
    pub valid_from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub valid_to: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePricingRule {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub percent_bp: i64,
    #[serde(default)]
    pub amount: Option<MoneyInput>,
    #[serde(default)]
    pub buy_quantity: u32,
    #[serde(default)]
    pub get_quantity: u32,
    #[serde(default)]
    pub min_quantity: u32,
    #[serde(default)]
    pub min_subtotal: Option<MoneyInput>,
    #[serde(default)]
    pub product_ids: Vec<String>,
    #[serde(default)]
    pub evaluation_order: i64,
    #[serde(default)]
    pub stackable: Option<bool>,
    #[serde(default)]
    pub exclusive: Option<bool>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub valid_from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub valid_to: Option<DateTime<Utc>>,
    #[serde(default)]
    pub currency: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePricingRule {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub percent_bp: Option<i64>,
    #[serde(default)]
    pub amount: Option<MoneyInput>,
    #[serde(default)]
    pub buy_quantity: Option<u32>,
    #[serde(default)]
    pub get_quantity: Option<u32>,
    #[serde(default)]
    pub min_quantity: Option<u32>,
    #[serde(default)]
    pub min_subtotal: Option<MoneyInput>,
    #[serde(default)]
    pub product_ids: Option<Vec<String>>,
    #[serde(default)]
    pub evaluation_order: Option<i64>,
    #[serde(default)]
    pub stackable: Option<bool>,
    #[serde(default)]
    pub exclusive: Option<bool>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub valid_from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub valid_to: Option<DateTime<Utc>>,
}

/// One coupon. Codes are stored uppercase and unique. A coupon is
/// a discount rule with a code, a validity window, a global usage
/// limit, and a per-customer usage limit; evaluation normalizes it
/// into rule shape so coupons and rules share one engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingCoupon {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub code: String,
    /// `percent`, `fixed`, or `buy_x_get_y`.
    pub kind: String,
    #[serde(default)]
    pub percent_bp: i64,
    /// Fixed amount in scaled minor units (kind `fixed` only).
    #[serde(default)]
    pub amount_scaled: i64,
    pub currency: String,
    #[serde(default)]
    pub buy_quantity: u32,
    #[serde(default)]
    pub get_quantity: u32,
    /// Empty means the coupon applies to every product.
    #[serde(default)]
    pub product_ids: Vec<String>,
    #[serde(default)]
    pub valid_from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub valid_to: Option<DateTime<Utc>>,
    /// Maximum redemptions across all customers; `None` is unlimited.
    #[serde(default)]
    pub usage_limit: Option<u32>,
    /// Maximum redemptions per customer; `None` is unlimited.
    #[serde(default)]
    pub per_customer_limit: Option<u32>,
    pub used_count: u32,
    pub stackable: bool,
    pub exclusive: bool,
    pub active: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePricingCoupon {
    pub code: String,
    pub kind: String,
    #[serde(default)]
    pub percent_bp: i64,
    #[serde(default)]
    pub amount: Option<MoneyInput>,
    #[serde(default)]
    pub buy_quantity: u32,
    #[serde(default)]
    pub get_quantity: u32,
    #[serde(default)]
    pub product_ids: Vec<String>,
    #[serde(default)]
    pub valid_from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub valid_to: Option<DateTime<Utc>>,
    #[serde(default)]
    pub usage_limit: Option<u32>,
    #[serde(default)]
    pub per_customer_limit: Option<u32>,
    #[serde(default)]
    pub stackable: Option<bool>,
    #[serde(default)]
    pub exclusive: Option<bool>,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub currency: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePricingCoupon {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub percent_bp: Option<i64>,
    #[serde(default)]
    pub amount: Option<MoneyInput>,
    #[serde(default)]
    pub buy_quantity: Option<u32>,
    #[serde(default)]
    pub get_quantity: Option<u32>,
    #[serde(default)]
    pub product_ids: Option<Vec<String>>,
    #[serde(default)]
    pub valid_from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub valid_to: Option<DateTime<Utc>>,
    #[serde(default)]
    pub usage_limit: Option<u32>,
    #[serde(default)]
    pub per_customer_limit: Option<u32>,
    #[serde(default)]
    pub stackable: Option<bool>,
    #[serde(default)]
    pub exclusive: Option<bool>,
    #[serde(default)]
    pub active: Option<bool>,
}

/// One customer's redemption count for one coupon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CouponRedemption {
    pub coupon_id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub customer_id: String,
    pub count: u32,
    pub last_redeemed_at: DateTime<Utc>,
}

/// One stored cost line of a recommendation (the durable form of
/// `CostLineInput`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingCostLine {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub recommendation_id: String,
    pub label: String,
    pub source: String,
    pub cost_scaled: i64,
    pub currency: String,
}

/// One price recommendation: a suggestion only. The engine never
/// sets a price; an admin confirms the suggestion, which creates
/// a price version in `draft`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingRecommendation {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub product_id: String,
    /// `cost_plus`, `tiered`, or `bundle`.
    pub kind: String,
    pub suggested_price_scaled: i64,
    pub currency: String,
    pub suggested_price: String,
    /// The full calculation, line by line.
    pub lines: Vec<CalculationLine>,
    /// The inputs that produced the recommendation, for audit.
    pub inputs: Value,
    /// `suggested`, `confirmed`, or `discarded`.
    pub status: String,
    pub created_by: String,
    #[serde(default)]
    pub confirmed_by: Option<String>,
    #[serde(default)]
    pub price_version_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateCostPlusRecommendation {
    pub product_id: String,
    pub costs: Vec<CostLineInput>,
    pub margin_bp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTieredRecommendation {
    pub product_id: String,
    pub tiers: Vec<TierInput>,
    pub quantity: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateBundleRecommendation {
    pub product_id: String,
    pub components: Vec<BundleComponentInput>,
    pub discount: BundleDiscountInput,
}

/// One stored tier of a tiered price definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingTier {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub product_id: String,
    pub min_quantity: u32,
    #[serde(default)]
    pub max_quantity: Option<u32>,
    pub unit_price_scaled: i64,
    pub currency: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePricingTier {
    pub product_id: String,
    pub min_quantity: u32,
    #[serde(default)]
    pub max_quantity: Option<u32>,
    pub unit_price: MoneyInput,
}

/// One bundle definition: a named set of products sold together.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingBundle {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub name: String,
    pub product_id: String,
    /// `percent` or `fixed`.
    pub discount_kind: String,
    #[serde(default)]
    pub discount_percent_bp: i64,
    #[serde(default)]
    pub discount_amount_scaled: i64,
    pub currency: String,
    pub active: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePricingBundle {
    pub name: String,
    pub product_id: String,
    pub discount_kind: String,
    #[serde(default)]
    pub discount_percent_bp: i64,
    #[serde(default)]
    pub discount_amount: Option<MoneyInput>,
    #[serde(default)]
    pub components: Vec<BundleComponentInput>,
    #[serde(default)]
    pub active: Option<bool>,
}

/// One component of a stored bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingBundleComponent {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub bundle_id: String,
    pub product_id: String,
    pub label: String,
    pub price_scaled: i64,
    pub currency: String,
}

/// One human-entered market reference price. The source and the
/// observation date are mandatory: a reference without provenance
/// is rejected. References are never scraped and never guessed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingReferencePrice {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub product_id: String,
    pub source: String,
    #[serde(default)]
    pub source_url: Option<String>,
    pub price_scaled: i64,
    pub currency: String,
    pub price: String,
    pub observed_at: DateTime<Utc>,
    #[serde(default)]
    pub note: String,
    pub entered_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateReferencePrice {
    pub product_id: String,
    pub source: String,
    #[serde(default)]
    pub source_url: Option<String>,
    pub price: MoneyInput,
    pub observed_at: DateTime<Utc>,
    #[serde(default)]
    pub note: Option<String>,
}

/// One versioned price for a product.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingPriceVersion {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub product_id: String,
    pub version: u32,
    pub price_scaled: i64,
    pub currency: String,
    pub price: String,
    /// Previous active price in scaled units; 0 when there was none.
    pub previous_price_scaled: i64,
    /// Change in basis points of the previous price; `None` when
    /// the previous price was zero and the new price is above zero
    /// (unmeasurable change: always needs a second approver).
    #[serde(default)]
    pub change_bp: Option<i64>,
    /// True when the change exceeded the approval threshold.
    pub above_threshold: bool,
    /// `draft`, `pending_approval`, `approved`, `active`, or `rejected`.
    pub status: String,
    pub requested_by: String,
    #[serde(default)]
    pub approved_by: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub recommendation_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePriceVersion {
    pub product_id: String,
    pub price: MoneyInput,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub recommendation_id: Option<String>,
}

/// One approval decision on a price version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingPriceApproval {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub price_version_id: String,
    pub approver: String,
    /// `approved` or `rejected`.
    pub decision: String,
    #[serde(default)]
    pub reason: Option<String>,
    pub decided_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PriceApprovalInput {
    #[serde(default)]
    pub reason: Option<String>,
}

/// One computed proration record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingProration {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    #[serde(default)]
    pub subscription_id: Option<String>,
    #[serde(default)]
    pub customer_id: Option<String>,
    pub kind: String,
    pub basis: String,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub change_at: DateTime<Utc>,
    pub period_days: i64,
    pub days_used: i64,
    pub days_remaining: i64,
    pub old_daily_rate_scaled: i64,
    pub new_daily_rate_scaled: i64,
    pub unused_credit_scaled: i64,
    pub new_charge_scaled: i64,
    pub net_due_scaled: i64,
    pub currency: String,
    pub lines: Vec<CalculationLine>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ComputeProrationRequest {
    #[serde(default)]
    pub subscription_id: Option<String>,
    #[serde(default)]
    pub customer_id: Option<String>,
    pub kind: String,
    pub old_price: MoneyInput,
    pub new_price: MoneyInput,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub change_at: DateTime<Utc>,
    #[serde(default)]
    pub basis: Option<String>,
}

/// One credit note: the only way to correct an issued invoice.
/// Credit notes are immutable once issued.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingCreditNote {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub number: String,
    pub invoice_id: String,
    pub amount_scaled: i64,
    pub currency: String,
    pub amount: String,
    pub lines: Vec<ReconcileAdjustmentInput>,
    pub reason: String,
    /// `draft` or `issued`.
    pub status: String,
    /// True when the note's own reconciliation matched at issue time.
    pub reconciled: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateCreditNote {
    pub invoice_id: String,
    pub amount: MoneyInput,
    pub lines: Vec<ReconcileAdjustmentInput>,
    pub reason: String,
    /// When true the note is issued immediately (and becomes
    /// immutable); otherwise it stays a draft.
    #[serde(default)]
    pub issue: Option<bool>,
}

/// One stored reconciliation run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingReconciliation {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    #[serde(default)]
    pub invoice_id: Option<String>,
    pub matched: bool,
    pub currency: String,
    pub components: Vec<ReconciliationComponent>,
    pub total_discrepancy_scaled: i64,
    pub checked_by: String,
    pub checked_at: DateTime<Utc>,
}

/// Request bodies for the discount, preview, and reconcile
/// endpoints.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscountEvaluateRequest {
    pub lines: Vec<DiscountLineRequest>,
    #[serde(default)]
    pub coupon_code: Option<String>,
    #[serde(default)]
    pub customer_id: Option<String>,
    #[serde(default)]
    pub currency: Option<String>,
    /// Extra rule ids to evaluate, in addition to every active
    /// rule. Active rules always run; listing one here is only
    /// needed when the caller wants a draft rule evaluated.
    #[serde(default)]
    pub rule_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiscountLineRequest {
    pub product_id: String,
    pub unit_price: MoneyInput,
    pub quantity: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InvoicePreviewRequest {
    #[serde(default)]
    pub invoice_id: Option<String>,
    pub lines: Vec<ReconcileLineRequest>,
    #[serde(default)]
    pub discounts: Vec<ReconcileAdjustmentInput>,
    #[serde(default)]
    pub proration_lines: Vec<ReconcileAdjustmentInput>,
    pub claimed: ReconcileClaimRequest,
    #[serde(default)]
    pub currency: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReconcileLineRequest {
    pub name: String,
    pub quantity: u32,
    pub unit_price: MoneyInput,
    #[serde(default)]
    pub tax_bp: i64,
    #[serde(default)]
    pub tax_inclusive: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReconcileClaimRequest {
    pub subtotal: MoneyInput,
    pub discounts: MoneyInput,
    pub proration: MoneyInput,
    pub tax_total: MoneyInput,
    pub total: MoneyInput,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConvertCurrencyRequest {
    pub amount: MoneyInput,
    pub target_currency: String,
}

/// Response of the AI narrative draft endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiDraftResponse {
    pub recommendation_id: String,
    pub draft: String,
    /// The numbers the draft uses, so a caller can verify that the
    /// text matches the engine output exactly.
    pub numbers: Value,
}

// State.

pub struct PricingState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    /// Pricing settings, one row per tenant, keyed by tenant id.
    pub(crate) settings: DashMap<String, PricingSettings>,
    pub(crate) currencies: DashMap<String, PricingCurrency>,
    pub(crate) rules: DashMap<String, PricingRule>,
    pub(crate) coupons: DashMap<String, PricingCoupon>,
    pub(crate) redemptions: DashMap<String, CouponRedemption>,
    pub(crate) recommendations: DashMap<String, PricingRecommendation>,
    pub(crate) cost_lines: DashMap<String, PricingCostLine>,
    pub(crate) tiers: DashMap<String, PricingTier>,
    pub(crate) bundles: DashMap<String, PricingBundle>,
    pub(crate) bundle_components: DashMap<String, PricingBundleComponent>,
    pub(crate) reference_prices: DashMap<String, PricingReferencePrice>,
    pub(crate) price_versions: DashMap<String, PricingPriceVersion>,
    pub(crate) price_approvals: DashMap<String, PricingPriceApproval>,
    pub(crate) prorations: DashMap<String, PricingProration>,
    pub(crate) credit_notes: DashMap<String, PricingCreditNote>,
    pub(crate) reconciliations: DashMap<String, PricingReconciliation>,
    pub(crate) credit_seq: RwLock<u64>,
}
