// procurement -- Vendor, purchase order, receipt, budget and request types.

use chrono::DateTime;
use chrono::NaiveDate;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Procurement / Vendor Management module -- vendors, purchase
// orders with line items and an approval workflow, goods receipt
// against purchase orders, spend tracking with per-category and
// per-vendor budgets, and vendor performance scoring.
//
// Routes live under `/api/v1/procurement`:
//
//   Vendors:
//     GET/POST /vendors -- list vendors / create a vendor.
//     GET/PATCH/DELETE /vendors/{id}.
//     GET  /vendors/{id}/performance -- on-time delivery rate,
//       spend, and quality notes for one vendor.
//   Purchase orders:
//     GET/POST /purchase-orders -- list purchase orders / create
//       one (draft; totals computed server-side).
//     GET/PATCH/DELETE /purchase-orders/{id}.
//     POST /purchase-orders/{id}/submit -- draft ->
//       pending_approval.
//     POST /purchase-orders/{id}/approve -- pending_approval ->
//       approved (approval row recorded).
//     POST /purchase-orders/{id}/reject -- pending_approval ->
//       draft with a recorded rejection reason (correct and
//       resubmit).
//     POST /purchase-orders/{id}/order -- approved -> ordered.
//     POST /purchase-orders/{id}/receive -- receive against a PO
//       line (partial receipt supported, over-receipt rejected).
//     POST /purchase-orders/{id}/close -- received /
//       partially_received -> closed.
//     POST /purchase-orders/{id}/cancel -- draft /
//       pending_approval / approved -> cancelled.
//     GET  /purchase-orders/{id}/approvals -- approval history.
//     GET  /purchase-orders/{id}/receipts -- receipts of a PO.
//   Purchase order lines (draft POs only):
//     GET/POST /purchase-orders/{id}/lines.
//     PATCH/DELETE /purchase-orders/{id}/lines/{line_id}.
//   Goods receipts:
//     GET  /receipts -- all receipts (paginated, filter by
//       purchase_order_id, vendor_id).
//   Budgets:
//     GET/POST /budgets -- list budgets / create a budget.
//     GET/PATCH/DELETE /budgets/{id} -- a budget read carries
//       spend vs budget (spent, remaining, over_budget).
//   Spend:
//     GET  /spend-summary -- committed spend totals with
//       per-category and per-vendor breakdowns and budget
//       comparison (filter by from / to / category / vendor_id).
//
// HONESTY -- READ THIS FIRST. What is real in this module:
// in-memory vendor, purchase-order, line, approval, receipt, and
// budget management; server-side money math (line totals and PO
// totals are always recomputed from quantity, unit price, and tax
// rate -- a client-supplied total is never trusted); the PO status
// machine (draft -> pending_approval -> approved -> ordered ->
// partially_received -> received -> closed / cancelled, with
// rejection returning the PO to draft); an approval workflow that
// records the approver, the approver role, the decision, and the
// reason for every approval and rejection; goods receipt that
// rejects over-receipt and derives the PO status from received
// quantities; spend tracking computed from committed POs
// (approved and later, never draft or cancelled) against
// per-category / per-vendor budgets, with over-budget warnings
// that never hard-block a purchase order; and vendor performance
// computed from promised vs received dates plus recorded quality
// notes and a manual rating. What is NOT built here: payments to
// vendors (the payments / billing modules own money movement --
// this module only records what was ordered and received);
// inventory stock movements (receipts record delivery, they do
// not post stock -- an inventory module owns that); automatic
// reordering; and actual email / push delivery (the notifications
// module owns delivery -- this module only emits
// `notification.requested` for over-budget warnings).
//
// Public-access note: there are no public routes in this module.
// Every route requires the `X-User-Id` and `X-Role` headers and
// a capability check (`procurement.view` for reads,
// `procurement.manage` for writes), exactly like the events
// module's authenticated routes.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `procurement.vendor.created`, `procurement.vendor.updated`,
//     `procurement.vendor.deleted`
//   - `procurement.po.created`, `procurement.po.submitted`,
//     `procurement.po.approved`, `procurement.po.rejected`,
//     `procurement.po.ordered`, `procurement.po.received`,
//     `procurement.po.closed`, `procurement.po.cancelled`,
//     `procurement.po.deleted`
//   - `procurement.budget.created`, `procurement.budget.updated`,
//     `procurement.budget.deleted`, `procurement.budget.warning`
//     (over-budget warning on approve / order; advisory only)
//   - `notification.requested` (over-budget warnings) with the
//     notifications module's payload shape.
// Events this module listens to: none.

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;

pub(crate) const MAX_TITLE_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_ADDRESS_LEN: usize = 500;

pub(crate) const MAX_EMAIL_LEN: usize = 254;

pub(crate) const MAX_PHONE_LEN: usize = 64;

pub(crate) const MAX_CATEGORY_LEN: usize = 80;

pub(crate) const MAX_TAG_LEN: usize = 64;

pub(crate) const MAX_TAGS: usize = 20;

pub(crate) const MAX_REFERENCE_LEN: usize = 200;

pub(crate) const MAX_NOTE_LEN: usize = 5000;

pub(crate) const MAX_REASON_LEN: usize = 2000;

pub(crate) const MAX_LINES_PER_PO: usize = 200;

pub(crate) const MAX_CATEGORIES_PER_VENDOR: usize = 20;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_PROCUREMENT_VIEW: &str = "procurement.view";

pub(crate) const PERM_PROCUREMENT_MANAGE: &str = "procurement.manage";

// Types

/// Vendor status: `active` vendors can receive new purchase
/// orders; `inactive` vendors are kept for history; `suspended`
/// vendors are blocked from new orders until reactivated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VendorStatus {
    Active,
    Inactive,
    Suspended,
}

impl Default for VendorStatus {
    fn default() -> Self {
        Self::Active
    }
}

impl VendorStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Inactive => "inactive",
            Self::Suspended => "suspended",
        }
    }
}

/// Lifecycle of a purchase order: `draft` -> `pending_approval`
/// -> `approved` -> `ordered` -> `partially_received` ->
/// `received` -> `closed`; `cancelled` is reachable from draft /
/// `pending_approval` / `approved`; a rejection returns the PO to
/// `draft` so it can be corrected and resubmitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PurchaseOrderStatus {
    Draft,
    PendingApproval,
    Approved,
    Ordered,
    PartiallyReceived,
    Received,
    Closed,
    Cancelled,
}

impl Default for PurchaseOrderStatus {
    fn default() -> Self {
        Self::Draft
    }
}

impl PurchaseOrderStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::PendingApproval => "pending_approval",
            Self::Approved => "approved",
            Self::Ordered => "ordered",
            Self::PartiallyReceived => "partially_received",
            Self::Received => "received",
            Self::Closed => "closed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Decision recorded on an approval row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approved,
    Rejected,
}

impl ApprovalDecision {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }
}

/// One vendor. `payment_reference` is a reference only (for
/// example "bank transfer -- details on file with finance"): raw
/// bank account details are never stored here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vendor {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub status: VendorStatus,
    /// Payment details reference only -- never raw bank secrets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment_reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Manual quality rating, 1..=5, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<i64>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One purchase order. `subtotal`, `tax_total`, and `total` are
/// always computed server-side from the lines and are never
/// accepted from the client. `po_number` is unique
/// (`PO-YYYY-NNNNNN`). Money is integer minor units; JSON carries
/// major units.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PurchaseOrder {
    pub id: Uuid,
    pub po_number: String,
    pub vendor_id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub currency: String,
    pub status: PurchaseOrderStatus,
    #[serde(with = "unified_core::money::serde_major")]
    pub subtotal: i64,
    #[serde(with = "unified_core::money::serde_major")]
    pub tax_total: i64,
    #[serde(with = "unified_core::money::serde_major")]
    pub total: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promised_date: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordered_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_reason: Option<String>,
    pub requested_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One purchase order line. `line_total` is computed server-side
/// as `quantity * unit_price * (1 + tax_rate)`; `received_quantity`
/// is maintained by goods receipts and never set directly. Money
/// is integer minor units; JSON carries major units. `quantity`
/// and `tax_rate` stay `f64`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PurchaseOrderLine {
    pub id: Uuid,
    pub purchase_order_id: Uuid,
    pub description: String,
    pub quantity: f64,
    #[serde(with = "unified_core::money::serde_major")]
    pub unit_price: i64,
    /// Tax rate as a fraction (0.10 = 10%).
    pub tax_rate: f64,
    #[serde(with = "unified_core::money::serde_major")]
    pub line_total: i64,
    pub received_quantity: f64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One approval workflow row: who decided, in which role, what
/// they decided, and why (a reason is required for rejections).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PurchaseOrderApproval {
    pub id: Uuid,
    pub purchase_order_id: Uuid,
    pub approver_id: String,
    pub approver_role: String,
    pub decision: ApprovalDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub decided_at: DateTime<Utc>,
}

/// One goods receipt: a quantity received against one PO line.
/// `quality_note` records the condition of the delivered goods.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoodsReceipt {
    pub id: Uuid,
    pub purchase_order_id: Uuid,
    pub line_id: Uuid,
    pub quantity_received: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_note: Option<String>,
    pub received_by: String,
    pub received_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// One spend budget, scoped to a category, a vendor, or both.
/// At least one scope must be set. Spend is computed on read
/// from committed purchase orders inside the period; an
/// over-budget state warns, it never blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcurementBudget {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<Uuid>,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    /// Budget amount in integer minor units; JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub budget_amount: i64,
    pub currency: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// Request bodies

#[derive(Debug, Deserialize)]
pub struct CreateVendorRequest {
    pub name: String,
    pub contact_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub address: Option<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub status: Option<VendorStatus>,
    pub payment_reference: Option<String>,
    pub notes: Option<String>,
    pub rating: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateVendorRequest {
    pub name: Option<String>,
    pub contact_name: Option<Option<String>>,
    pub email: Option<Option<String>>,
    pub phone: Option<Option<String>>,
    pub address: Option<Option<String>>,
    pub categories: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    pub status: Option<VendorStatus>,
    pub payment_reference: Option<Option<String>>,
    pub notes: Option<Option<String>>,
    pub rating: Option<Option<i64>>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePurchaseOrderRequest {
    pub vendor_id: Uuid,
    pub title: String,
    pub category: Option<String>,
    pub currency: Option<String>,
    pub promised_date: Option<NaiveDate>,
    #[serde(default)]
    pub lines: Vec<CreateLineRequest>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePurchaseOrderRequest {
    pub title: Option<String>,
    pub category: Option<Option<String>>,
    pub currency: Option<String>,
    pub promised_date: Option<Option<NaiveDate>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CreateLineRequest {
    pub description: String,
    pub quantity: f64,
    /// Unit price in integer minor units; JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub unit_price: i64,
    #[serde(default)]
    pub tax_rate: f64,
}

#[derive(Debug, Deserialize)]
pub struct UpdateLineRequest {
    pub description: Option<String>,
    pub quantity: Option<f64>,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub unit_price: Option<i64>,
    pub tax_rate: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct ReceiveRequest {
    pub line_id: Uuid,
    pub quantity: f64,
    pub quality_note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RejectRequest {
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct ApproveRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CancelRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateBudgetRequest {
    pub category: Option<String>,
    pub vendor_id: Option<Uuid>,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    /// Budget amount in integer minor units; JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub budget_amount: i64,
    pub currency: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateBudgetRequest {
    pub category: Option<Option<String>>,
    pub vendor_id: Option<Option<Uuid>>,
    pub period_start: Option<NaiveDate>,
    pub period_end: Option<NaiveDate>,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub budget_amount: Option<i64>,
    pub currency: Option<String>,
}
