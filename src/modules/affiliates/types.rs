// affiliates module - Domain types.
//
// Split from the former single-file module `src/modules/affiliates.rs`.
// The module root (`affiliates.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::helpers::caller;

// Types

/// Approval lifecycle of an affiliate: `pending` -> `approved` /
/// `rejected`; `approved` -> `suspended` -> `approved` (reactivate).
/// A rejected affiliate may re-apply by creating a new record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AffiliateStatus {
    Pending,
    Approved,
    Rejected,
    Suspended,
}

impl AffiliateStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Suspended => "suspended",
        }
    }
}

/// How a commission is computed: a percentage of the order total,
/// or a fixed amount in the affiliate currency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommissionType {
    Percent,
    Fixed,
}

impl CommissionType {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Percent => "percent",
            Self::Fixed => "fixed",
        }
    }
}

/// Lifecycle of a commission: `pending` -> `approved` -> `paid`,
/// or `void` (manual, with a reason) from pending / approved.
/// Approval and voiding are always manual, capability-checked
/// actions; nothing here auto-approves or auto-pays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommissionStatus {
    Pending,
    Approved,
    Paid,
    Void,
}

impl CommissionStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Paid => "paid",
            Self::Void => "void",
        }
    }
}

/// Lifecycle of a payout request: `requested` -> `approved` ->
/// `paid`, or `rejected` from requested / approved. The payout
/// itself (moving money) happens in the billing module, which
/// listens to `affiliates.payout.requested`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayoutStatus {
    Requested,
    Approved,
    Paid,
    Rejected,
}

impl PayoutStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Approved => "approved",
            Self::Paid => "paid",
            Self::Rejected => "rejected",
        }
    }
}

/// One affiliate (partner). `user_id` is the affiliate's own user
/// id (from the `X-User-Id` header); `commission_type` /
/// `commission_value` are the global default (percent 0..=100, or
/// a fixed amount); per-product overrides live in
/// `AffiliateCommissionRule`. `attribution_window_days` bounds
/// last-click attribution for this affiliate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Affiliate {
    pub id: Uuid,
    pub user_id: String,
    pub display_name: String,
    pub email: String,
    pub status: AffiliateStatus,
    pub commission_type: CommissionType,
    /// Percent (0..=100) when `commission_type` is percent, fixed
    /// amount in major units otherwise. This field is deliberately
    /// NOT minor units: a percent such as 12.5 cannot be expressed
    /// as minor units, and the fixed case keeps the major-unit
    /// value the API has always carried. Fixed amounts are
    /// converted to minor units when a commission is computed
    /// (see `commission_for`).
    pub commission_value: f64,
    pub currency: String,
    pub attribution_window_days: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payout_method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payout_reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Per-product commission override. `product_id` is the store
/// product id; the affiliate default applies when no rule matches
/// a product. At most one rule per (affiliate, product).
/// `commission_value` follows the same dual rule as on
/// `Affiliate`: percent 0..=100, or a fixed amount in major units.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AffiliateCommissionRule {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub product_id: String,
    pub commission_type: CommissionType,
    pub commission_value: f64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One referral link / code. `code` is globally unique (the value
/// visitors arrive with); `campaign` groups links; `product_id`
/// narrows a link to one product when set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AffiliateLink {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub campaign: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One recorded click on a referral link. The visitor IP is stored
/// only as a one-way SHA-256 hex digest (`ip_hash`); the raw IP is
/// never stored or logged. `flagged` with `flag_reasons` is
/// advisory: a flagged click is still counted, and flagging never
/// blocks, bans, or voids anything on its own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AffiliateClick {
    pub id: Uuid,
    pub link_id: Uuid,
    pub affiliate_id: Uuid,
    /// SHA-256 hex of the visitor IP (plus nothing else); raw IPs
    /// are never stored.
    pub ip_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referrer: Option<String>,
    pub flagged: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flag_reasons: Vec<String>,
    pub converted: bool,
    pub clicked_at: DateTime<Utc>,
}

/// One tracked conversion: `signup` (a referred user registered)
/// or `purchase` (a referred order completed). `flagged` marks
/// conversions that need human review (for example self-referral:
/// the customer is the affiliate themselves).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AffiliateConversion {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click_id: Option<Uuid>,
    pub customer_user_id: String,
    /// `signup` or `purchase`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    /// Order total for purchases in minor units, 0 for signups.
    /// JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub order_total: i64,
    pub currency: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    pub flagged: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flag_reasons: Vec<String>,
    pub attributed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// One commission earned from a purchase conversion. Created in
/// `pending` and moved only by manual, capability-checked actions.
/// `fraud_flags` mirrors the conversion flags so a reviewer sees
/// why this commission needs a look before approving it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AffiliateCommission {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub conversion_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    pub commission_type: CommissionType,
    /// Percent applied, or the fixed amount in major units, at
    /// creation time. Dual units, like `commission_value`: percent
    /// 0..=100 for a percent commission, major-unit amount for a
    /// fixed one. Deliberately NOT minor units.
    pub rate_or_amount: f64,
    /// Amount payable in minor units. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub amount: i64,
    pub currency: String,
    pub status: CommissionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payout_id: Option<Uuid>,
    pub flagged: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fraud_flags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One payout request: a set of approved commissions paid out
/// together. This module only requests; the billing module moves
/// the money and the payout is marked paid here afterwards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AffiliatePayout {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commission_ids: Vec<Uuid>,
    /// Sum of the commissions in minor units. JSON carries major
    /// units.
    #[serde(with = "unified_core::money::serde_major")]
    pub total_amount: i64,
    pub currency: String,
    pub status: PayoutStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    pub requested_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Affiliate application / creation input. `user_id` may only be
/// set by a manager creating an affiliate for someone else; a
/// plain caller always applies as themselves.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateAffiliateRequest {
    #[serde(default)]
    pub user_id: Option<String>,
    pub display_name: String,
    pub email: String,
    #[serde(default)]
    pub commission_type: Option<CommissionType>,
    #[serde(default)]
    pub commission_value: Option<f64>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub attribution_window_days: Option<i64>,
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub payout_method: Option<String>,
    #[serde(default)]
    pub payout_reference: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateAffiliateRequest {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub commission_type: Option<CommissionType>,
    #[serde(default)]
    pub commission_value: Option<f64>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub attribution_window_days: Option<i64>,
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub payout_method: Option<String>,
    #[serde(default)]
    pub payout_reference: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReviewRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateLinkRequest {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub campaign: Option<String>,
    #[serde(default)]
    pub product_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateCommissionRuleRequest {
    pub product_id: String,
    pub commission_type: CommissionType,
    pub commission_value: f64,
}

/// Public click-tracking input. `ip` is accepted only as a
/// fallback when no `X-Forwarded-For` header is present (for
/// example behind a proxy that strips it); the value is hashed
/// immediately and never stored.
#[derive(Debug, Clone, Deserialize)]
pub struct TrackClickRequest {
    pub code: String,
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub referrer: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateConversionRequest {
    pub affiliate_id: String,
    #[serde(default)]
    pub link_id: Option<String>,
    #[serde(default)]
    pub click_id: Option<String>,
    pub customer_user_id: String,
    pub kind: String,
    #[serde(default)]
    pub order_id: Option<String>,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub order_total: Option<i64>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub product_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RequestPayoutRequest {
    #[serde(default)]
    pub commission_ids: Option<Vec<String>>,
    #[serde(default)]
    pub method: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MarkPaidRequest {
    #[serde(default)]
    pub reference: Option<String>,
}

/// In-memory stores for affiliates. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000044_affiliates`; until that migration is
/// executed, affiliates do not survive a restart.
#[derive(Clone)]
pub struct AffiliatesState {
    pub affiliates: Arc<DashMap<Uuid, Affiliate>>,
    pub commission_rules: Arc<DashMap<Uuid, AffiliateCommissionRule>>,
    pub links: Arc<DashMap<Uuid, AffiliateLink>>,
    pub clicks: Arc<DashMap<Uuid, AffiliateClick>>,
    pub conversions: Arc<DashMap<Uuid, AffiliateConversion>>,
    pub commissions: Arc<DashMap<Uuid, AffiliateCommission>>,
    pub payouts: Arc<DashMap<Uuid, AffiliatePayout>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for AffiliatesState {
    fn default() -> Self {
        Self {
            affiliates: Arc::new(DashMap::new()),
            commission_rules: Arc::new(DashMap::new()),
            links: Arc::new(DashMap::new()),
            clicks: Arc::new(DashMap::new()),
            conversions: Arc::new(DashMap::new()),
            commissions: Arc::new(DashMap::new()),
            payouts: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl AffiliatesState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affiliate_status_names_are_snake_case() {
        assert_eq!(AffiliateStatus::Pending.as_str(), "pending");
        assert_eq!(AffiliateStatus::Approved.as_str(), "approved");
        assert_eq!(AffiliateStatus::Rejected.as_str(), "rejected");
        assert_eq!(AffiliateStatus::Suspended.as_str(), "suspended");
    }

    #[test]
    fn commission_type_names_are_snake_case() {
        assert_eq!(CommissionType::Percent.as_str(), "percent");
        assert_eq!(CommissionType::Fixed.as_str(), "fixed");
    }

    #[test]
    fn commission_status_names_are_snake_case() {
        assert_eq!(CommissionStatus::Pending.as_str(), "pending");
        assert_eq!(CommissionStatus::Approved.as_str(), "approved");
        assert_eq!(CommissionStatus::Paid.as_str(), "paid");
        assert_eq!(CommissionStatus::Void.as_str(), "void");
    }

    #[test]
    fn payout_status_names_are_snake_case() {
        assert_eq!(PayoutStatus::Requested.as_str(), "requested");
        assert_eq!(PayoutStatus::Approved.as_str(), "approved");
        assert_eq!(PayoutStatus::Paid.as_str(), "paid");
        assert_eq!(PayoutStatus::Rejected.as_str(), "rejected");
    }
}
