// Procurement / Vendor Management module — vendors, purchase
// orders with line items and an approval workflow, goods receipt
// against purchase orders, spend tracking with per-category and
// per-vendor budgets, and vendor performance scoring.
//
// Routes live under `/api/v1/procurement`:
//
//   Vendors:
//     GET/POST /vendors — list vendors / create a vendor.
//     GET/PATCH/DELETE /vendors/{id}.
//     GET  /vendors/{id}/performance — on-time delivery rate,
//       spend, and quality notes for one vendor.
//   Purchase orders:
//     GET/POST /purchase-orders — list purchase orders / create
//       one (draft; totals computed server-side).
//     GET/PATCH/DELETE /purchase-orders/{id}.
//     POST /purchase-orders/{id}/submit — draft ->
//       pending_approval.
//     POST /purchase-orders/{id}/approve — pending_approval ->
//       approved (approval row recorded).
//     POST /purchase-orders/{id}/reject — pending_approval ->
//       draft with a recorded rejection reason (correct and
//       resubmit).
//     POST /purchase-orders/{id}/order — approved -> ordered.
//     POST /purchase-orders/{id}/receive — receive against a PO
//       line (partial receipt supported, over-receipt rejected).
//     POST /purchase-orders/{id}/close — received /
//       partially_received -> closed.
//     POST /purchase-orders/{id}/cancel — draft /
//       pending_approval / approved -> cancelled.
//     GET  /purchase-orders/{id}/approvals — approval history.
//     GET  /purchase-orders/{id}/receipts — receipts of a PO.
//   Purchase order lines (draft POs only):
//     GET/POST /purchase-orders/{id}/lines.
//     PATCH/DELETE /purchase-orders/{id}/lines/{line_id}.
//   Goods receipts:
//     GET  /receipts — all receipts (paginated, filter by
//       purchase_order_id, vendor_id).
//   Budgets:
//     GET/POST /budgets — list budgets / create a budget.
//     GET/PATCH/DELETE /budgets/{id} — a budget read carries
//       spend vs budget (spent, remaining, over_budget).
//   Spend:
//     GET  /spend-summary — committed spend totals with
//       per-category and per-vendor breakdowns and budget
//       comparison (filter by from / to / category / vendor_id).
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory vendor, purchase-order, line, approval, receipt, and
// budget management; server-side money math (line totals and PO
// totals are always recomputed from quantity, unit price, and tax
// rate — a client-supplied total is never trusted); the PO status
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
// vendors (the payments / billing modules own money movement —
// this module only records what was ordered and received);
// inventory stock movements (receipts record delivery, they do
// not post stock — an inventory module owns that); automatic
// reordering; and actual email / push delivery (the notifications
// module owns delivery — this module only emits
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

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::HookBus;
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_NAME_LEN: usize = 200;
const MAX_TITLE_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 5000;
const MAX_ADDRESS_LEN: usize = 500;
const MAX_EMAIL_LEN: usize = 254;
const MAX_PHONE_LEN: usize = 64;
const MAX_CATEGORY_LEN: usize = 80;
const MAX_TAG_LEN: usize = 64;
const MAX_TAGS: usize = 20;
const MAX_REFERENCE_LEN: usize = 200;
const MAX_NOTE_LEN: usize = 5000;
const MAX_REASON_LEN: usize = 2000;
const MAX_LINES_PER_PO: usize = 200;
const MAX_CATEGORIES_PER_VENDOR: usize = 20;

/// Capability names checked through the `role.check` hook.
const PERM_PROCUREMENT_VIEW: &str = "procurement.view";
const PERM_PROCUREMENT_MANAGE: &str = "procurement.manage";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

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
    fn as_str(&self) -> &'static str {
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
    fn as_str(&self) -> &'static str {
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
    fn as_str(&self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }
}

/// One vendor. `payment_reference` is a reference only (for
/// example "bank transfer — details on file with finance"): raw
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
    /// Payment details reference only — never raw bank secrets.
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
/// (`PO-YYYY-NNNNNN`).
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
    pub subtotal: f64,
    pub tax_total: f64,
    pub total: f64,
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
/// is maintained by goods receipts and never set directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PurchaseOrderLine {
    pub id: Uuid,
    pub purchase_order_id: Uuid,
    pub description: String,
    pub quantity: f64,
    pub unit_price: f64,
    /// Tax rate as a fraction (0.10 = 10%).
    pub tax_rate: f64,
    pub line_total: f64,
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
    pub budget_amount: f64,
    pub currency: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

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
    pub unit_price: f64,
    #[serde(default)]
    pub tax_rate: f64,
}

#[derive(Debug, Deserialize)]
pub struct UpdateLineRequest {
    pub description: Option<String>,
    pub quantity: Option<f64>,
    pub unit_price: Option<f64>,
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
    pub budget_amount: f64,
    pub currency: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateBudgetRequest {
    pub category: Option<Option<String>>,
    pub vendor_id: Option<Option<Uuid>>,
    pub period_start: Option<NaiveDate>,
    pub period_end: Option<NaiveDate>,
    pub budget_amount: Option<f64>,
    pub currency: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for procurement. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000049_procurement`; until that migration is
/// executed, procurement data does not survive a restart.
#[derive(Clone)]
pub struct ProcurementState {
    pub vendors: Arc<DashMap<Uuid, Vendor>>,
    pub purchase_orders: Arc<DashMap<Uuid, PurchaseOrder>>,
    pub lines: Arc<DashMap<Uuid, PurchaseOrderLine>>,
    pub approvals: Arc<DashMap<Uuid, PurchaseOrderApproval>>,
    pub receipts: Arc<DashMap<Uuid, GoodsReceipt>>,
    pub budgets: Arc<DashMap<Uuid, ProcurementBudget>>,
    pub po_counter: Arc<RwLock<u64>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for ProcurementState {
    fn default() -> Self {
        Self {
            vendors: Arc::new(DashMap::new()),
            purchase_orders: Arc::new(DashMap::new()),
            lines: Arc::new(DashMap::new()),
            approvals: Arc::new(DashMap::new()),
            receipts: Arc::new(DashMap::new()),
            budgets: Arc::new(DashMap::new()),
            po_counter: Arc::new(RwLock::new(0)),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl ProcurementState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape.
    fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "procurement",
                "category": "procurement",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    /// Next unique purchase order number (`PO-YYYY-NNNNNN`).
    fn next_po_number(&self) -> String {
        let mut counter = self.po_counter.write().expect("po counter lock poisoned");
        *counter += 1;
        format!("PO-{}-{:06}", Utc::now().year(), *counter)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the ai module.
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the ai module's capability check).
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn valid_email(email: &str) -> bool {
    if email.is_empty() || email.len() > MAX_EMAIL_LEN || email.contains(' ') {
        return false;
    }
    let mut parts = email.split('@');
    let (Some(local), Some(domain)) = (parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() || local.is_empty() || domain.is_empty() {
        return false;
    }
    domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// Money: finite, non-negative, rounded to 4 decimal places.
fn valid_price(price: f64) -> UnifiedResult<f64> {
    if !price.is_finite() || price < 0.0 {
        return Err(UnifiedError::bad_request("price must be a finite number >= 0"));
    }
    Ok((price * 10_000.0).round() / 10_000.0)
}

fn round_money(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// Currency: 3-letter ISO code, uppercased.
fn valid_currency(currency: &str) -> UnifiedResult<String> {
    let upper = currency.trim().to_uppercase();
    if upper.len() != 3 || !upper.chars().all(|c| c.is_ascii_uppercase()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code such as \"USD\"",
        ));
    }
    Ok(upper)
}

fn valid_quantity(quantity: f64) -> UnifiedResult<f64> {
    if !quantity.is_finite() || quantity <= 0.0 {
        return Err(UnifiedError::bad_request("quantity must be a finite number > 0"));
    }
    Ok((quantity * 10_000.0).round() / 10_000.0)
}

fn valid_tax_rate(tax_rate: f64) -> UnifiedResult<f64> {
    if !tax_rate.is_finite() || tax_rate < 0.0 || tax_rate > 1.0 {
        return Err(UnifiedError::bad_request("tax_rate must be between 0 and 1 (0.10 = 10%)"));
    }
    Ok((tax_rate * 10_000.0).round() / 10_000.0)
}

fn validate_tags(tags: &[String]) -> UnifiedResult<Vec<String>> {
    if tags.len() > MAX_TAGS {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_TAGS} tags are allowed"
        )));
    }
    let mut out: Vec<String> = Vec::new();
    for tag in tags {
        let cleaned = require_text(tag, "tag", MAX_TAG_LEN)?;
        if out.iter().any(|entry| entry == &cleaned) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate tag \"{cleaned}\""
            )));
        }
        out.push(cleaned);
    }
    Ok(out)
}

fn validate_categories(categories: &[String]) -> UnifiedResult<Vec<String>> {
    if categories.len() > MAX_CATEGORIES_PER_VENDOR {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_CATEGORIES_PER_VENDOR} categories are allowed"
        )));
    }
    let mut out: Vec<String> = Vec::new();
    for category in categories {
        let cleaned = require_text(category, "category", MAX_CATEGORY_LEN)?;
        if out.iter().any(|entry| entry == &cleaned) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate category \"{cleaned}\""
            )));
        }
        out.push(cleaned);
    }
    Ok(out)
}

fn validate_rating(rating: i64) -> UnifiedResult<i64> {
    if !(1..=5).contains(&rating) {
        return Err(UnifiedError::bad_request("rating must be between 1 and 5"));
    }
    Ok(rating)
}

fn load_vendor(state: &ProcurementState, raw_id: &str) -> UnifiedResult<Vendor> {
    let id = parse_uuid(raw_id, "vendor")?;
    state
        .vendors
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Vendor not found: {id}")))
}

fn load_po(state: &ProcurementState, raw_id: &str) -> UnifiedResult<PurchaseOrder> {
    let id = parse_uuid(raw_id, "purchase order")?;
    state
        .purchase_orders
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Purchase order not found: {id}")))
}

fn load_budget(state: &ProcurementState, raw_id: &str) -> UnifiedResult<ProcurementBudget> {
    let id = parse_uuid(raw_id, "budget")?;
    state
        .budgets
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Budget not found: {id}")))
}

fn lines_of(state: &ProcurementState, po_id: Uuid) -> Vec<PurchaseOrderLine> {
    let mut lines: Vec<PurchaseOrderLine> = state
        .lines
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|line| line.purchase_order_id == po_id)
        .collect();
    lines.sort_by_key(|line| line.created_at);
    lines
}

fn load_line(state: &ProcurementState, po_id: Uuid, raw_id: &str) -> UnifiedResult<PurchaseOrderLine> {
    let id = parse_uuid(raw_id, "purchase order line")?;
    state
        .lines
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|line| line.purchase_order_id == po_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Purchase order line not found: {id}")))
}

fn approvals_of(state: &ProcurementState, po_id: Uuid) -> Vec<PurchaseOrderApproval> {
    let mut approvals: Vec<PurchaseOrderApproval> = state
        .approvals
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|approval| approval.purchase_order_id == po_id)
        .collect();
    approvals.sort_by_key(|approval| approval.decided_at);
    approvals
}

fn receipts_of_po(state: &ProcurementState, po_id: Uuid) -> Vec<GoodsReceipt> {
    let mut receipts: Vec<GoodsReceipt> = state
        .receipts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|receipt| receipt.purchase_order_id == po_id)
        .collect();
    receipts.sort_by_key(|receipt| receipt.received_at);
    receipts
}

/// Recompute a PO's money from its lines. `line_total` is
/// `quantity * unit_price * (1 + tax_rate)`; the PO subtotal is
/// the sum of `quantity * unit_price`, the tax total the sum of
/// the tax parts, and the total their sum. The result is stored
/// back on the PO row.
fn recompute_po_totals(state: &ProcurementState, po: &mut PurchaseOrder) {
    let mut subtotal = 0.0;
    let mut tax_total = 0.0;
    for line in lines_of(state, po.id) {
        let net = line.quantity * line.unit_price;
        subtotal += net;
        tax_total += net * line.tax_rate;
    }
    po.subtotal = round_money(subtotal);
    po.tax_total = round_money(tax_total);
    po.total = round_money(subtotal + tax_total);
}

/// Derive the receipt status of a PO from its lines: no receipts
/// yet keeps the current status; some lines partially received
/// (or some fully received while others are not) means
/// `partially_received`; every line fully received means
/// `received` (and stamps `received_at`).
fn derive_receipt_status(state: &ProcurementState, po: &mut PurchaseOrder) {
    let lines = lines_of(state, po.id);
    if lines.is_empty() {
        return;
    }
    let any_received = lines.iter().any(|line| line.received_quantity > 0.0);
    if !any_received {
        return;
    }
    let all_received = lines
        .iter()
        .all(|line| line.received_quantity >= line.quantity);
    let now = Utc::now();
    if all_received {
        po.status = PurchaseOrderStatus::Received;
        if po.received_at.is_none() {
            po.received_at = Some(now);
        }
    } else {
        po.status = PurchaseOrderStatus::PartiallyReceived;
    }
    po.updated_at = now;
}

/// Validate a PO status transition. Returns a 409 conflict
/// naming the illegal move.
fn transition(
    po: &PurchaseOrder,
    target: PurchaseOrderStatus,
    action: &str,
) -> UnifiedResult<PurchaseOrderStatus> {
    let allowed = match po.status {
        PurchaseOrderStatus::Draft => [
            PurchaseOrderStatus::PendingApproval,
            PurchaseOrderStatus::Cancelled,
        ]
        .contains(&target),
        PurchaseOrderStatus::PendingApproval => [
            PurchaseOrderStatus::Approved,
            PurchaseOrderStatus::Draft,
            PurchaseOrderStatus::Cancelled,
        ]
        .contains(&target),
        PurchaseOrderStatus::Approved => [
            PurchaseOrderStatus::Ordered,
            PurchaseOrderStatus::Cancelled,
        ]
        .contains(&target),
        PurchaseOrderStatus::Ordered => [
            PurchaseOrderStatus::PartiallyReceived,
            PurchaseOrderStatus::Received,
        ]
        .contains(&target),
        PurchaseOrderStatus::PartiallyReceived => {
            [PurchaseOrderStatus::Received, PurchaseOrderStatus::Closed].contains(&target)
        }
        PurchaseOrderStatus::Received => [PurchaseOrderStatus::Closed].contains(&target),
        PurchaseOrderStatus::Closed | PurchaseOrderStatus::Cancelled => false,
    };
    if !allowed {
        return Err(UnifiedError::conflict(format!(
            "Cannot {action} a purchase order in status \"{}\" (target \"{}\")",
            po.status.as_str(),
            target.as_str(),
        )));
    }
    Ok(target)
}

/// A PO counts as committed spend once approved (and later):
/// draft, pending approval, and cancelled POs never count.
fn is_committed(status: PurchaseOrderStatus) -> bool {
    matches!(
        status,
        PurchaseOrderStatus::Approved
            | PurchaseOrderStatus::Ordered
            | PurchaseOrderStatus::PartiallyReceived
            | PurchaseOrderStatus::Received
            | PurchaseOrderStatus::Closed
    )
}

/// Committed spend for one scope inside a date window. `from` /
/// `to` bound the PO creation date; `category` and `vendor_id`
/// narrow the scope (either may be `None` for "any").
fn committed_spend(
    state: &ProcurementState,
    category: Option<&str>,
    vendor_id: Option<Uuid>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> f64 {
    let mut total = 0.0;
    for entry in state.purchase_orders.iter() {
        let po = entry.value();
        if !is_committed(po.status) {
            continue;
        }
        if let Some(category) = category {
            if po.category.as_deref() != Some(category) {
                continue;
            }
        }
        if let Some(vendor_id) = vendor_id {
            if po.vendor_id != vendor_id {
                continue;
            }
        }
        let created = po.created_at.date_naive();
        if let Some(from) = from {
            if created < from {
                continue;
            }
        }
        if let Some(to) = to {
            if created > to {
                continue;
            }
        }
        total += po.total;
    }
    round_money(total)
}

/// Budgets that apply to a PO: a budget applies when its
/// category matches the PO category (or the budget has no
/// category) AND its vendor matches the PO vendor (or the budget
/// has no vendor), and the PO creation date falls inside the
/// budget period.
fn applicable_budgets(state: &ProcurementState, po: &PurchaseOrder) -> Vec<ProcurementBudget> {
    let created = po.created_at.date_naive();
    state
        .budgets
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|budget| {
            if let Some(category) = &budget.category {
                if po.category.as_deref() != Some(category.as_str()) {
                    return false;
                }
            }
            if let Some(vendor_id) = budget.vendor_id {
                if po.vendor_id != vendor_id {
                    return false;
                }
            }
            created >= budget.period_start && created <= budget.period_end
        })
        .collect()
}

/// Over-budget warnings for one PO: every applicable budget whose
/// committed spend already exceeds its amount. Advisory only —
/// the caller never blocks on this.
fn budget_warnings(state: &ProcurementState, po: &PurchaseOrder) -> Vec<Value> {
    let mut warnings: Vec<Value> = Vec::new();
    for budget in applicable_budgets(state, po) {
        let spent = committed_spend(
            state,
            budget.category.as_deref(),
            budget.vendor_id,
            Some(budget.period_start),
            Some(budget.period_end),
        );
        if spent > budget.budget_amount {
            warnings.push(json!({
                "budget_id": budget.id.to_string(),
                "category": budget.category,
                "vendor_id": budget.vendor_id.map(|id| id.to_string()),
                "budget_amount": budget.budget_amount,
                "spent": spent,
                "over_by": round_money(spent - budget.budget_amount),
                "currency": budget.currency,
            }));
        }
    }
    warnings
}

/// Emit a `procurement.budget.warning` event and a notification
/// for each over-budget warning. Advisory only.
fn warn_over_budget(state: &ProcurementState, po: &PurchaseOrder, warnings: &[Value]) {
    for warning in warnings {
        state.emit(
            "procurement.budget.warning",
            json!({
                "purchase_order_id": po.id.to_string(),
                "po_number": po.po_number,
                "budget_id": warning.get("budget_id"),
                "spent": warning.get("spent"),
                "budget_amount": warning.get("budget_amount"),
                "over_by": warning.get("over_by"),
            }),
        );
        state.notify(
            "procurement.budget_warning",
            &po.requested_by,
            &format!("Over budget: purchase order {}", po.po_number),
            &format!(
                "Purchase order {} ({}) pushes committed spend to {} against a budget of {} {}.",
                po.po_number,
                po.title,
                warning.get("spent").and_then(Value::as_f64).unwrap_or(0.0),
                warning.get("budget_amount").and_then(Value::as_f64).unwrap_or(0.0),
                warning.get("currency").and_then(Value::as_str).unwrap_or("")
            ),
            Some(format!("/api/v1/procurement/purchase-orders/{}", po.id)),
        );
    }
}

/// Public view of a vendor.
fn vendor_view(vendor: &Vendor) -> Value {
    json!(vendor)
}

/// Public view of a purchase order, with its lines embedded and
/// any current over-budget warnings attached.
fn po_view(state: &ProcurementState, po: &PurchaseOrder) -> Value {
    let lines: Vec<Value> = lines_of(state, po.id)
        .into_iter()
        .map(|line| json!(line))
        .collect();
    let warnings = if is_committed(po.status) {
        budget_warnings(state, po)
    } else {
        Vec::new()
    };
    json!({
        "id": po.id.to_string(),
        "po_number": po.po_number,
        "vendor_id": po.vendor_id.to_string(),
        "title": po.title,
        "category": po.category,
        "currency": po.currency,
        "status": po.status.as_str(),
        "subtotal": po.subtotal,
        "tax_total": po.tax_total,
        "total": po.total,
        "promised_date": po.promised_date.map(|date| date.to_string()),
        "ordered_at": po.ordered_at.map(|value| value.to_rfc3339()),
        "received_at": po.received_at.map(|value| value.to_rfc3339()),
        "cancel_reason": po.cancel_reason,
        "requested_by": po.requested_by,
        "approved_by": po.approved_by,
        "lines": lines,
        "budget_warnings": warnings,
        "created_at": po.created_at.to_rfc3339(),
        "updated_at": po.updated_at.to_rfc3339(),
    })
}

/// Public view of a budget, with spend vs budget computed on read.
fn budget_view(state: &ProcurementState, budget: &ProcurementBudget) -> Value {
    let spent = committed_spend(
        state,
        budget.category.as_deref(),
        budget.vendor_id,
        Some(budget.period_start),
        Some(budget.period_end),
    );
    let remaining = round_money(budget.budget_amount - spent);
    json!({
        "id": budget.id.to_string(),
        "category": budget.category,
        "vendor_id": budget.vendor_id.map(|id| id.to_string()),
        "period_start": budget.period_start.to_string(),
        "period_end": budget.period_end.to_string(),
        "budget_amount": budget.budget_amount,
        "currency": budget.currency,
        "spent": spent,
        "remaining": remaining,
        "over_budget": spent > budget.budget_amount,
        "created_by": budget.created_by,
        "created_at": budget.created_at.to_rfc3339(),
        "updated_at": budget.updated_at.to_rfc3339(),
    })
}

// ---------------------------------------------------------------------------
// Vendor handlers
// ---------------------------------------------------------------------------

async fn list_vendors(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let category_filter = params.filters.get("category").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Value> = state
            .vendors
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|vendor| {
                if let Some(status) = &status_filter {
                    if vendor.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(category) = &category_filter {
                    if !vendor.categories.iter().any(|entry| entry == category) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {}",
                        vendor.name,
                        vendor.contact_name.clone().unwrap_or_default(),
                        vendor.email.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .map(|vendor| vendor_view(&vendor))
            .collect();
        let sort = params.allowed_sort(&["name", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a
                    .get("name")
                    .and_then(Value::as_str)
                    .cmp(&b.get("name").and_then(Value::as_str)),
                _ => a
                    .get("created_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("created_at").and_then(Value::as_str)),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_vendor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    body: web::Json<CreateVendorRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let contact_name = match &input.contact_name {
            Some(raw) => Some(require_text(raw, "contact_name", MAX_NAME_LEN)?),
            None => None,
        };
        let email = match &input.email {
            Some(raw) => {
                let cleaned = require_text(raw, "email", MAX_EMAIL_LEN)?;
                if !valid_email(&cleaned) {
                    return Err(UnifiedError::bad_request(
                        "email must be a valid email address",
                    ));
                }
                Some(cleaned)
            }
            None => None,
        };
        let phone = match &input.phone {
            Some(raw) => Some(require_text(raw, "phone", MAX_PHONE_LEN)?),
            None => None,
        };
        let address = match &input.address {
            Some(raw) => Some(require_text(raw, "address", MAX_ADDRESS_LEN)?),
            None => None,
        };
        let categories = validate_categories(&input.categories)?;
        let tags = validate_tags(&input.tags)?;
        let payment_reference = match &input.payment_reference {
            Some(raw) => Some(require_text(raw, "payment_reference", MAX_REFERENCE_LEN)?),
            None => None,
        };
        let notes = match &input.notes {
            Some(raw) => Some(require_text(raw, "notes", MAX_NOTE_LEN)?),
            None => None,
        };
        let rating = match input.rating {
            Some(value) => Some(validate_rating(value)?),
            None => None,
        };
        let now = Utc::now();
        let vendor = Vendor {
            id: Uuid::now_v7(),
            name,
            contact_name,
            email,
            phone,
            address,
            categories,
            tags,
            status: input.status.unwrap_or_default(),
            payment_reference,
            notes,
            rating,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.vendors.insert(vendor.id, vendor.clone());
        state.emit(
            "procurement.vendor.created",
            json!({
                "vendor_id": vendor.id.to_string(),
                "name": vendor.name,
                "status": vendor.status.as_str(),
                "created_by": caller.actor,
            }),
        );
        Ok(vendor_view(&vendor))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_vendor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_VIEW)?;
        let vendor = load_vendor(&state, &path.into_inner())?;
        Ok(vendor_view(&vendor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_vendor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateVendorRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let mut vendor = load_vendor(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(name) = &input.name {
            vendor.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(contact_name) = &input.contact_name {
            vendor.contact_name = match contact_name {
                Some(raw) => Some(require_text(raw, "contact_name", MAX_NAME_LEN)?),
                None => None,
            };
        }
        if let Some(email) = &input.email {
            vendor.email = match email {
                Some(raw) => {
                    let cleaned = require_text(raw, "email", MAX_EMAIL_LEN)?;
                    if !valid_email(&cleaned) {
                        return Err(UnifiedError::bad_request(
                            "email must be a valid email address",
                        ));
                    }
                    Some(cleaned)
                }
                None => None,
            };
        }
        if let Some(phone) = &input.phone {
            vendor.phone = match phone {
                Some(raw) => Some(require_text(raw, "phone", MAX_PHONE_LEN)?),
                None => None,
            };
        }
        if let Some(address) = &input.address {
            vendor.address = match address {
                Some(raw) => Some(require_text(raw, "address", MAX_ADDRESS_LEN)?),
                None => None,
            };
        }
        if let Some(categories) = &input.categories {
            vendor.categories = validate_categories(categories)?;
        }
        if let Some(tags) = &input.tags {
            vendor.tags = validate_tags(tags)?;
        }
        if let Some(status) = input.status {
            vendor.status = status;
        }
        if let Some(payment_reference) = &input.payment_reference {
            vendor.payment_reference = match payment_reference {
                Some(raw) => Some(require_text(raw, "payment_reference", MAX_REFERENCE_LEN)?),
                None => None,
            };
        }
        if let Some(notes) = &input.notes {
            vendor.notes = match notes {
                Some(raw) => Some(require_text(raw, "notes", MAX_NOTE_LEN)?),
                None => None,
            };
        }
        if let Some(rating) = &input.rating {
            vendor.rating = match rating {
                Some(value) => Some(validate_rating(*value)?),
                None => None,
            };
        }
        vendor.updated_at = Utc::now();
        state.vendors.insert(vendor.id, vendor.clone());
        state.emit(
            "procurement.vendor.updated",
            json!({
                "vendor_id": vendor.id.to_string(),
                "name": vendor.name,
                "updated_by": caller.actor,
            }),
        );
        Ok(vendor_view(&vendor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_vendor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let vendor = load_vendor(&state, &path.into_inner())?;
        if state
            .purchase_orders
            .iter()
            .any(|entry| entry.value().vendor_id == vendor.id)
        {
            return Err(UnifiedError::conflict(
                "Vendor has purchase orders and cannot be deleted; set status to inactive instead",
            ));
        }
        state.vendors.remove(&vendor.id);
        state.emit(
            "procurement.vendor.deleted",
            json!({
                "vendor_id": vendor.id.to_string(),
                "name": vendor.name,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "vendor_id": vendor.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Vendor performance: on-time delivery rate computed from PO
/// promised dates vs receipt dates, total committed spend, PO
/// counts, and recent quality notes from receipts.
async fn vendor_performance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_VIEW)?;
        let vendor = load_vendor(&state, &path.into_inner())?;
        let pos: Vec<PurchaseOrder> = state
            .purchase_orders
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|po| po.vendor_id == vendor.id)
            .collect();
        let total_pos = pos.len() as i64;
        let committed_pos = pos.iter().filter(|po| is_committed(po.status)).count() as i64;
        let total_spend = round_money(
            pos.iter()
                .filter(|po| is_committed(po.status))
                .map(|po| po.total)
                .sum(),
        );
        // On-time rate: receipts of this vendor's POs that have a
        // promised date, where the receipt date is on or before it.
        let mut on_time = 0i64;
        let mut measured = 0i64;
        let mut quality_notes: Vec<Value> = Vec::new();
        for po in &pos {
            let Some(promised) = po.promised_date else {
                continue;
            };
            for receipt in receipts_of_po(&state, po.id) {
                measured += 1;
                if receipt.received_at.date_naive() <= promised {
                    on_time += 1;
                }
                if let Some(note) = &receipt.quality_note {
                    quality_notes.push(json!({
                        "purchase_order_id": po.id.to_string(),
                        "po_number": po.po_number,
                        "receipt_id": receipt.id.to_string(),
                        "note": note,
                        "received_at": receipt.received_at.to_rfc3339(),
                    }));
                }
            }
        }
        quality_notes.sort_by(|a, b| {
            b.get("received_at")
                .and_then(Value::as_str)
                .cmp(&a.get("received_at").and_then(Value::as_str))
        });
        quality_notes.truncate(20);
        let on_time_rate = if measured > 0 {
            (on_time as f64 / measured as f64 * 10_000.0).round() / 10_000.0
        } else {
            0.0
        };
        Ok(json!({
            "vendor_id": vendor.id.to_string(),
            "name": vendor.name,
            "status": vendor.status.as_str(),
            "rating": vendor.rating,
            "total_purchase_orders": total_pos,
            "committed_purchase_orders": committed_pos,
            "total_committed_spend": total_spend,
            "receipts_measured": measured,
            "on_time_receipts": on_time,
            "on_time_rate": on_time_rate,
            "quality_notes": quality_notes,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Purchase order handlers
// ---------------------------------------------------------------------------

async fn list_purchase_orders(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let vendor_filter = params.filters.get("vendor_id").cloned();
        let category_filter = params.filters.get("category").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Value> = state
            .purchase_orders
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|po| {
                if let Some(status) = &status_filter {
                    if po.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(vendor_id) = &vendor_filter {
                    if po.vendor_id.to_string() != *vendor_id {
                        return false;
                    }
                }
                if let Some(category) = &category_filter {
                    if po.category.as_deref() != Some(category.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {}", po.po_number, po.title).to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .map(|po| po_view(&state, &po))
            .collect();
        let sort = params.allowed_sort(&["po_number", "total", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "po_number" => a
                    .get("po_number")
                    .and_then(Value::as_str)
                    .cmp(&b.get("po_number").and_then(Value::as_str)),
                "total" => a
                    .get("total")
                    .and_then(Value::as_f64)
                    .partial_cmp(&b.get("total").and_then(Value::as_f64).unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal),
                _ => a
                    .get("created_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("created_at").and_then(Value::as_str)),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

fn validate_line_input(input: &CreateLineRequest) -> UnifiedResult<(String, f64, f64, f64)> {
    let description = require_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
    let quantity = valid_quantity(input.quantity)?;
    let unit_price = valid_price(input.unit_price)?;
    let tax_rate = valid_tax_rate(input.tax_rate)?;
    Ok((description, quantity, unit_price, tax_rate))
}

async fn create_purchase_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    body: web::Json<CreatePurchaseOrderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let input = body.into_inner();
        let vendor = state
            .vendors
            .get(&input.vendor_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Vendor not found: {}", input.vendor_id))
            })?;
        if vendor.status == VendorStatus::Suspended {
            return Err(UnifiedError::conflict(format!(
                "Vendor \"{}\" is suspended and cannot receive new purchase orders",
                vendor.name
            )));
        }
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let category = match &input.category {
            Some(raw) => Some(require_text(raw, "category", MAX_CATEGORY_LEN)?),
            None => None,
        };
        let currency = valid_currency(input.currency.as_deref().unwrap_or("USD"))?;
        if input.lines.len() > MAX_LINES_PER_PO {
            return Err(UnifiedError::bad_request(format!(
                "at most {MAX_LINES_PER_PO} lines per purchase order"
            )));
        }
        let mut validated_lines: Vec<(String, f64, f64, f64)> = Vec::new();
        for line in &input.lines {
            validated_lines.push(validate_line_input(line)?);
        }
        let now = Utc::now();
        let mut po = PurchaseOrder {
            id: Uuid::now_v7(),
            po_number: state.next_po_number(),
            vendor_id: vendor.id,
            title,
            category,
            currency,
            status: PurchaseOrderStatus::Draft,
            subtotal: 0.0,
            tax_total: 0.0,
            total: 0.0,
            promised_date: input.promised_date,
            ordered_at: None,
            received_at: None,
            cancel_reason: None,
            requested_by: caller.actor.clone(),
            approved_by: None,
            created_at: now,
            updated_at: now,
        };
        state.purchase_orders.insert(po.id, po.clone());
        for (description, quantity, unit_price, tax_rate) in validated_lines {
            let line = PurchaseOrderLine {
                id: Uuid::now_v7(),
                purchase_order_id: po.id,
                description,
                quantity,
                unit_price,
                tax_rate,
                line_total: round_money(quantity * unit_price * (1.0 + tax_rate)),
                received_quantity: 0.0,
                created_at: now,
                updated_at: now,
            };
            state.lines.insert(line.id, line);
        }
        recompute_po_totals(&state, &mut po);
        state.purchase_orders.insert(po.id, po.clone());
        state.emit(
            "procurement.po.created",
            json!({
                "purchase_order_id": po.id.to_string(),
                "po_number": po.po_number,
                "vendor_id": po.vendor_id.to_string(),
                "total": po.total,
                "currency": po.currency,
                "status": "draft",
                "requested_by": caller.actor,
            }),
        );
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_purchase_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_VIEW)?;
        let po = load_po(&state, &path.into_inner())?;
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_purchase_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePurchaseOrderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let mut po = load_po(&state, &path.into_inner())?;
        if po.status != PurchaseOrderStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Only a draft purchase order can be edited (current status \"{}\")",
                po.status.as_str(),
            )));
        }
        let input = body.into_inner();
        if let Some(title) = &input.title {
            po.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(category) = &input.category {
            po.category = match category {
                Some(raw) => Some(require_text(raw, "category", MAX_CATEGORY_LEN)?),
                None => None,
            };
        }
        if let Some(currency) = &input.currency {
            po.currency = valid_currency(currency)?;
        }
        if let Some(promised_date) = input.promised_date {
            po.promised_date = promised_date;
        }
        po.updated_at = Utc::now();
        state.purchase_orders.insert(po.id, po.clone());
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_purchase_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let po = load_po(&state, &path.into_inner())?;
        if po.status != PurchaseOrderStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Only a draft purchase order can be deleted (current status \"{}\"). Cancel it instead.",
                po.status.as_str(),
            )));
        }
        for line in lines_of(&state, po.id) {
            state.lines.remove(&line.id);
        }
        state
            .approvals
            .retain(|_, entry| entry.purchase_order_id != po.id);
        state
            .receipts
            .retain(|_, entry| entry.purchase_order_id != po.id);
        state.purchase_orders.remove(&po.id);
        state.emit(
            "procurement.po.deleted",
            json!({
                "purchase_order_id": po.id.to_string(),
                "po_number": po.po_number,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "purchase_order_id": po.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn submit_purchase_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let mut po = load_po(&state, &path.into_inner())?;
        transition(&po, PurchaseOrderStatus::PendingApproval, "submit")?;
        if lines_of(&state, po.id).is_empty() {
            return Err(UnifiedError::bad_request(
                "Add at least one line before submitting a purchase order",
            ));
        }
        po.status = PurchaseOrderStatus::PendingApproval;
        po.updated_at = Utc::now();
        state.purchase_orders.insert(po.id, po.clone());
        state.emit(
            "procurement.po.submitted",
            json!({
                "purchase_order_id": po.id.to_string(),
                "po_number": po.po_number,
                "total": po.total,
                "submitted_by": caller.actor,
            }),
        );
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn approve_purchase_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ApproveRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let mut po = load_po(&state, &path.into_inner())?;
        transition(&po, PurchaseOrderStatus::Approved, "approve")?;
        let now = Utc::now();
        let reason = match &body.reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        let approval = PurchaseOrderApproval {
            id: Uuid::now_v7(),
            purchase_order_id: po.id,
            approver_id: caller.actor.clone(),
            approver_role: caller.role.clone(),
            decision: ApprovalDecision::Approved,
            reason: reason.clone(),
            decided_at: now,
        };
        state.approvals.insert(approval.id, approval);
        po.status = PurchaseOrderStatus::Approved;
        po.approved_by = Some(caller.actor.clone());
        po.updated_at = now;
        state.purchase_orders.insert(po.id, po.clone());
        // Over-budget warning (advisory only — never blocks).
        let warnings = budget_warnings(&state, &po);
        warn_over_budget(&state, &po, &warnings);
        state.emit(
            "procurement.po.approved",
            json!({
                "purchase_order_id": po.id.to_string(),
                "po_number": po.po_number,
                "total": po.total,
                "approved_by": caller.actor,
                "approver_role": caller.role,
                "budget_warnings": warnings.len(),
            }),
        );
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn reject_purchase_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RejectRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let mut po = load_po(&state, &path.into_inner())?;
        transition(&po, PurchaseOrderStatus::Draft, "reject")?;
        let reason = require_text(&body.reason, "reason", MAX_REASON_LEN)?;
        let now = Utc::now();
        let approval = PurchaseOrderApproval {
            id: Uuid::now_v7(),
            purchase_order_id: po.id,
            approver_id: caller.actor.clone(),
            approver_role: caller.role.clone(),
            decision: ApprovalDecision::Rejected,
            reason: Some(reason.clone()),
            decided_at: now,
        };
        state.approvals.insert(approval.id, approval);
        // Rejection returns the PO to draft so it can be corrected
        // and resubmitted; the rejection stays on the record.
        po.status = PurchaseOrderStatus::Draft;
        po.approved_by = None;
        po.updated_at = now;
        state.purchase_orders.insert(po.id, po.clone());
        state.emit(
            "procurement.po.rejected",
            json!({
                "purchase_order_id": po.id.to_string(),
                "po_number": po.po_number,
                "rejected_by": caller.actor,
                "approver_role": caller.role,
                "reason": reason,
            }),
        );
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn order_purchase_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let mut po = load_po(&state, &path.into_inner())?;
        transition(&po, PurchaseOrderStatus::Ordered, "order")?;
        let now = Utc::now();
        po.status = PurchaseOrderStatus::Ordered;
        po.ordered_at = Some(now);
        po.updated_at = now;
        state.purchase_orders.insert(po.id, po.clone());
        let warnings = budget_warnings(&state, &po);
        warn_over_budget(&state, &po, &warnings);
        state.emit(
            "procurement.po.ordered",
            json!({
                "purchase_order_id": po.id.to_string(),
                "po_number": po.po_number,
                "total": po.total,
                "ordered_by": caller.actor,
                "budget_warnings": warnings.len(),
            }),
        );
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn receive_purchase_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReceiveRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let mut po = load_po(&state, &path.into_inner())?;
        if !matches!(
            po.status,
            PurchaseOrderStatus::Ordered | PurchaseOrderStatus::PartiallyReceived
        ) {
            return Err(UnifiedError::conflict(format!(
                "Goods can only be received for an ordered or partially received purchase order (current status \"{}\")",
                po.status.as_str(),
            )));
        }
        let input = body.into_inner();
        let mut line = load_line(&state, po.id, &input.line_id.to_string())?;
        let quantity = valid_quantity(input.quantity)?;
        // Over-receipt is rejected: received + new must not exceed
        // the ordered quantity.
        if line.received_quantity + quantity > line.quantity {
            return Err(UnifiedError::bad_request(format!(
                "Over-receipt rejected: line ordered {:.4}, already received {:.4}, attempted {:.4}",
                line.quantity, line.received_quantity, quantity
            )));
        }
        let quality_note = match &input.quality_note {
            Some(raw) => Some(require_text(raw, "quality_note", MAX_NOTE_LEN)?),
            None => None,
        };
        let now = Utc::now();
        line.received_quantity = round_money(line.received_quantity + quantity);
        line.updated_at = now;
        state.lines.insert(line.id, line.clone());
        let receipt = GoodsReceipt {
            id: Uuid::now_v7(),
            purchase_order_id: po.id,
            line_id: line.id,
            quantity_received: quantity,
            quality_note,
            received_by: caller.actor.clone(),
            received_at: now,
            created_at: now,
        };
        state.receipts.insert(receipt.id, receipt.clone());
        let previous_status = po.status;
        derive_receipt_status(&state, &mut po);
        state.purchase_orders.insert(po.id, po.clone());
        state.emit(
            "procurement.po.received",
            json!({
                "purchase_order_id": po.id.to_string(),
                "po_number": po.po_number,
                "receipt_id": receipt.id.to_string(),
                "line_id": line.id.to_string(),
                "quantity_received": quantity,
                "previous_status": previous_status.as_str(),
                "status": po.status.as_str(),
                "received_by": caller.actor,
            }),
        );
        Ok(json!({
            "purchase_order": po_view(&state, &po),
            "receipt": receipt,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn close_purchase_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let mut po = load_po(&state, &path.into_inner())?;
        transition(&po, PurchaseOrderStatus::Closed, "close")?;
        po.status = PurchaseOrderStatus::Closed;
        po.updated_at = Utc::now();
        state.purchase_orders.insert(po.id, po.clone());
        state.emit(
            "procurement.po.closed",
            json!({
                "purchase_order_id": po.id.to_string(),
                "po_number": po.po_number,
                "closed_by": caller.actor,
            }),
        );
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn cancel_purchase_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CancelRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let mut po = load_po(&state, &path.into_inner())?;
        transition(&po, PurchaseOrderStatus::Cancelled, "cancel")?;
        let reason = match &body.reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        po.status = PurchaseOrderStatus::Cancelled;
        po.cancel_reason = reason.clone();
        po.updated_at = Utc::now();
        state.purchase_orders.insert(po.id, po.clone());
        state.emit(
            "procurement.po.cancelled",
            json!({
                "purchase_order_id": po.id.to_string(),
                "po_number": po.po_number,
                "cancelled_by": caller.actor,
                "reason": reason,
            }),
        );
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_po_approvals(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_VIEW)?;
        let po = load_po(&state, &path.into_inner())?;
        let params = query.into_inner();
        let decision_filter = params.filters.get("decision").cloned();
        let items: Vec<Value> = approvals_of(&state, po.id)
            .into_iter()
            .filter(|approval| {
                if let Some(decision) = &decision_filter {
                    if approval.decision.as_str() != decision.as_str() {
                        return false;
                    }
                }
                true
            })
            .map(|approval| json!(approval))
            .collect();
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_po_receipts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_VIEW)?;
        let po = load_po(&state, &path.into_inner())?;
        let params = query.into_inner();
        let items: Vec<Value> = receipts_of_po(&state, po.id)
            .into_iter()
            .map(|receipt| json!(receipt))
            .collect();
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Purchase order line handlers (draft POs only)
// ---------------------------------------------------------------------------

async fn list_po_lines(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_VIEW)?;
        let po = load_po(&state, &path.into_inner())?;
        let params = query.into_inner();
        let items: Vec<Value> = lines_of(&state, po.id)
            .into_iter()
            .map(|line| json!(line))
            .collect();
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn add_po_line(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateLineRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let mut po = load_po(&state, &path.into_inner())?;
        if po.status != PurchaseOrderStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Lines can only be added to a draft purchase order (current status \"{}\")",
                po.status.as_str(),
            )));
        }
        if lines_of(&state, po.id).len() >= MAX_LINES_PER_PO {
            return Err(UnifiedError::bad_request(format!(
                "at most {MAX_LINES_PER_PO} lines per purchase order"
            )));
        }
        let (description, quantity, unit_price, tax_rate) = validate_line_input(&body.into_inner())?;
        let now = Utc::now();
        let line = PurchaseOrderLine {
            id: Uuid::now_v7(),
            purchase_order_id: po.id,
            description,
            quantity,
            unit_price,
            tax_rate,
            line_total: round_money(quantity * unit_price * (1.0 + tax_rate)),
            received_quantity: 0.0,
            created_at: now,
            updated_at: now,
        };
        state.lines.insert(line.id, line.clone());
        recompute_po_totals(&state, &mut po);
        po.updated_at = now;
        state.purchase_orders.insert(po.id, po.clone());
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn update_po_line(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateLineRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let (po_id, line_id) = path.into_inner();
        let mut po = load_po(&state, &po_id)?;
        if po.status != PurchaseOrderStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Lines can only be edited on a draft purchase order (current status \"{}\")",
                po.status.as_str(),
            )));
        }
        let mut line = load_line(&state, po.id, &line_id)?;
        let input = body.into_inner();
        if let Some(description) = &input.description {
            line.description = require_text(description, "description", MAX_DESCRIPTION_LEN)?;
        }
        if let Some(quantity) = input.quantity {
            line.quantity = valid_quantity(quantity)?;
        }
        if let Some(unit_price) = input.unit_price {
            line.unit_price = valid_price(unit_price)?;
        }
        if let Some(tax_rate) = input.tax_rate {
            line.tax_rate = valid_tax_rate(tax_rate)?;
        }
        line.line_total = round_money(line.quantity * line.unit_price * (1.0 + line.tax_rate));
        line.updated_at = Utc::now();
        state.lines.insert(line.id, line);
        recompute_po_totals(&state, &mut po);
        po.updated_at = Utc::now();
        state.purchase_orders.insert(po.id, po.clone());
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_po_line(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let (po_id, line_id) = path.into_inner();
        let mut po = load_po(&state, &po_id)?;
        if po.status != PurchaseOrderStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Lines can only be removed from a draft purchase order (current status \"{}\")",
                po.status.as_str(),
            )));
        }
        let line = load_line(&state, po.id, &line_id)?;
        state.lines.remove(&line.id);
        recompute_po_totals(&state, &mut po);
        po.updated_at = Utc::now();
        state.purchase_orders.insert(po.id, po.clone());
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Receipt, budget, and spend handlers
// ---------------------------------------------------------------------------

async fn list_receipts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_VIEW)?;
        let params = query.into_inner();
        let po_filter = params.filters.get("purchase_order_id").cloned();
        let vendor_filter = params.filters.get("vendor_id").cloned();
        let vendor_pos: Option<Vec<Uuid>> = match &vendor_filter {
            Some(raw) => {
                let vendor_id = parse_uuid(raw, "vendor")?;
                Some(
                    state
                        .purchase_orders
                        .iter()
                        .map(|entry| entry.value().clone())
                        .filter(|po| po.vendor_id == vendor_id)
                        .map(|po| po.id)
                        .collect(),
                )
            }
            None => None,
        };
        let items: Vec<Value> = state
            .receipts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|receipt| {
                if let Some(po_id) = &po_filter {
                    if receipt.purchase_order_id.to_string() != *po_id {
                        return false;
                    }
                }
                if let Some(po_ids) = &vendor_pos {
                    if !po_ids.contains(&receipt.purchase_order_id) {
                        return false;
                    }
                }
                true
            })
            .map(|receipt| json!(receipt))
            .collect();
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_budgets(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_VIEW)?;
        let params = query.into_inner();
        let category_filter = params.filters.get("category").cloned();
        let vendor_filter = params.filters.get("vendor_id").cloned();
        let over_filter = params.filters.get("over_budget").cloned();
        let mut items: Vec<Value> = state
            .budgets
            .iter()
            .map(|entry| entry.value().clone())
            .map(|budget| budget_view(&state, &budget))
            .filter(|view| {
                if let Some(category) = &category_filter {
                    if view.get("category").and_then(Value::as_str) != Some(category.as_str()) {
                        return false;
                    }
                }
                if let Some(vendor_id) = &vendor_filter {
                    if view.get("vendor_id").and_then(Value::as_str) != Some(vendor_id.as_str()) {
                        return false;
                    }
                }
                if let Some(over) = &over_filter {
                    let want = over == "true";
                    if view.get("over_budget").and_then(Value::as_bool).unwrap_or(false) != want {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| {
            a.get("period_start")
                .and_then(Value::as_str)
                .cmp(&b.get("period_start").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

fn validate_budget_scope(
    category: &Option<String>,
    vendor_id: &Option<Uuid>,
    period_start: NaiveDate,
    period_end: NaiveDate,
    budget_amount: f64,
) -> UnifiedResult<(Option<String>, Option<Uuid>, f64)> {
    if category.is_none() && vendor_id.is_none() {
        return Err(UnifiedError::bad_request(
            "A budget must scope to a category, a vendor, or both",
        ));
    }
    let category = match category {
        Some(raw) => Some(require_text(raw, "category", MAX_CATEGORY_LEN)?),
        None => None,
    };
    if period_end < period_start {
        return Err(UnifiedError::bad_request(
            "period_end must be on or after period_start",
        ));
    }
    let amount = valid_price(budget_amount)?;
    Ok((category, *vendor_id, amount))
}

async fn create_budget(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    body: web::Json<CreateBudgetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let input = body.into_inner();
        if let Some(vendor_id) = input.vendor_id {
            if !state.vendors.contains_key(&vendor_id) {
                return Err(UnifiedError::not_found(format!("Vendor not found: {vendor_id}")));
            }
        }
        let (category, vendor_id, amount) = validate_budget_scope(
            &input.category,
            &input.vendor_id,
            input.period_start,
            input.period_end,
            input.budget_amount,
        )?;
        let currency = valid_currency(input.currency.as_deref().unwrap_or("USD"))?;
        let now = Utc::now();
        let budget = ProcurementBudget {
            id: Uuid::now_v7(),
            category,
            vendor_id,
            period_start: input.period_start,
            period_end: input.period_end,
            budget_amount: amount,
            currency,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.budgets.insert(budget.id, budget.clone());
        state.emit(
            "procurement.budget.created",
            json!({
                "budget_id": budget.id.to_string(),
                "category": budget.category,
                "vendor_id": budget.vendor_id.map(|id| id.to_string()),
                "budget_amount": budget.budget_amount,
                "created_by": caller.actor,
            }),
        );
        Ok(budget_view(&state, &budget))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_budget(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_VIEW)?;
        let budget = load_budget(&state, &path.into_inner())?;
        Ok(budget_view(&state, &budget))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_budget(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateBudgetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let mut budget = load_budget(&state, &path.into_inner())?;
        let input = body.into_inner();
        let new_category = match &input.category {
            Some(value) => match value {
                Some(raw) => Some(require_text(raw, "category", MAX_CATEGORY_LEN)?),
                None => None,
            },
            None => budget.category.clone(),
        };
        let new_vendor_id = match input.vendor_id {
            Some(value) => value,
            None => budget.vendor_id,
        };
        if let Some(vendor_id) = new_vendor_id {
            if !state.vendors.contains_key(&vendor_id) {
                return Err(UnifiedError::not_found(format!("Vendor not found: {vendor_id}")));
            }
        }
        let new_start = input.period_start.unwrap_or(budget.period_start);
        let new_end = input.period_end.unwrap_or(budget.period_end);
        let new_amount = match input.budget_amount {
            Some(amount) => valid_price(amount)?,
            None => budget.budget_amount,
        };
        let (category, vendor_id, amount) = validate_budget_scope(
            &new_category,
            &new_vendor_id,
            new_start,
            new_end,
            new_amount,
        )?;
        budget.category = category;
        budget.vendor_id = vendor_id;
        budget.period_start = new_start;
        budget.period_end = new_end;
        budget.budget_amount = amount;
        if let Some(currency) = &input.currency {
            budget.currency = valid_currency(currency)?;
        }
        budget.updated_at = Utc::now();
        state.budgets.insert(budget.id, budget.clone());
        state.emit(
            "procurement.budget.updated",
            json!({
                "budget_id": budget.id.to_string(),
                "budget_amount": budget.budget_amount,
                "updated_by": caller.actor,
            }),
        );
        Ok(budget_view(&state, &budget))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_budget(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let budget = load_budget(&state, &path.into_inner())?;
        state.budgets.remove(&budget.id);
        state.emit(
            "procurement.budget.deleted",
            json!({
                "budget_id": budget.id.to_string(),
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "budget_id": budget.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn spend_summary(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_VIEW)?;
        let params = query.into_inner();
        let from = params
            .filters
            .get("from")
            .and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok());
        let to = params
            .filters
            .get("to")
            .and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok());
        if let (Some(from), Some(to)) = (from, to) {
            if to < from {
                return Err(UnifiedError::bad_request("to must be on or after from"));
            }
        }
        let category_filter = params.filters.get("category").cloned();
        let vendor_filter = params
            .filters
            .get("vendor_id")
            .and_then(|raw| Uuid::parse_str(raw).ok());
        let total = committed_spend(
            &state,
            category_filter.as_deref(),
            vendor_filter,
            from,
            to,
        );
        // Per-category breakdown.
        let mut by_category: Vec<Value> = Vec::new();
        let mut categories: Vec<String> = state
            .purchase_orders
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|po| is_committed(po.status))
            .filter_map(|po| po.category.clone())
            .collect();
        categories.sort();
        categories.dedup();
        for category in categories {
            if let Some(filter) = &category_filter {
                if &category != filter {
                    continue;
                }
            }
            let spent = committed_spend(&state, Some(&category), vendor_filter, from, to);
            if spent > 0.0 {
                by_category.push(json!({ "category": category, "spent": spent }));
            }
        }
        // Per-vendor breakdown.
        let mut by_vendor: Vec<Value> = Vec::new();
        let mut vendor_ids: Vec<Uuid> = state
            .purchase_orders
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|po| is_committed(po.status))
            .map(|po| po.vendor_id)
            .collect();
        vendor_ids.sort();
        vendor_ids.dedup();
        for vendor_id in vendor_ids {
            if let Some(filter) = vendor_filter {
                if vendor_id != filter {
                    continue;
                }
            }
            let spent = committed_spend(&state, category_filter.as_deref(), Some(vendor_id), from, to);
            if spent > 0.0 {
                let name = state
                    .vendors
                    .get(&vendor_id)
                    .map(|entry| entry.value().name.clone());
                by_vendor.push(json!({
                    "vendor_id": vendor_id.to_string(),
                    "vendor_name": name,
                    "spent": spent,
                }));
            }
        }
        // Budget comparison for budgets overlapping the window.
        let budgets: Vec<Value> = state
            .budgets
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|budget| {
                if let Some(from) = from {
                    if budget.period_end < from {
                        return false;
                    }
                }
                if let Some(to) = to {
                    if budget.period_start > to {
                        return false;
                    }
                }
                if let Some(category) = &category_filter {
                    if budget.category.as_deref() != Some(category.as_str()) {
                        return false;
                    }
                }
                if let Some(vendor_id) = vendor_filter {
                    if budget.vendor_id != Some(vendor_id) {
                        return false;
                    }
                }
                true
            })
            .map(|budget| budget_view(&state, &budget))
            .collect();
        Ok(json!({
            "total_committed_spend": total,
            "from": from.map(|date| date.to_string()),
            "to": to.map(|date| date.to_string()),
            "by_category": by_category,
            "by_vendor": by_vendor,
            "budgets": budgets,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct ProcurementModule {
    state: ProcurementState,
}

impl ProcurementModule {
    pub fn new() -> Self {
        Self {
            state: ProcurementState::default(),
        }
    }
}

impl Default for ProcurementModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ProcurementModule {
    fn id(&self) -> &'static str {
        "procurement"
    }

    fn name(&self) -> &'static str {
        "Procurement / Vendor Management"
    }

    fn description(&self) -> &'static str {
        "Vendors, purchase orders with server-side totals and an approval workflow, goods receipt against PO lines with over-receipt rejection, spend tracking with per-category and per-vendor budgets (over-budget warnings never block), and vendor performance from promised vs received dates. Vendor payments and inventory stock movements are not handled here."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications"]
    }

    /// Procurement is sold as a product: a customer buys a module
    /// entitlement for it (see `crate::modules::licensing`).
    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/procurement")
                .app_data(state)
                .route("/vendors", web::get().to(list_vendors))
                .route("/vendors", web::post().to(create_vendor))
                .route("/vendors/{id}", web::get().to(get_vendor))
                .route("/vendors/{id}", web::patch().to(update_vendor))
                .route("/vendors/{id}", web::delete().to(delete_vendor))
                .route(
                    "/vendors/{id}/performance",
                    web::get().to(vendor_performance),
                )
                .route("/purchase-orders", web::get().to(list_purchase_orders))
                .route("/purchase-orders", web::post().to(create_purchase_order))
                .route("/purchase-orders/{id}", web::get().to(get_purchase_order))
                .route(
                    "/purchase-orders/{id}",
                    web::patch().to(update_purchase_order),
                )
                .route(
                    "/purchase-orders/{id}",
                    web::delete().to(delete_purchase_order),
                )
                .route(
                    "/purchase-orders/{id}/submit",
                    web::post().to(submit_purchase_order),
                )
                .route(
                    "/purchase-orders/{id}/approve",
                    web::post().to(approve_purchase_order),
                )
                .route(
                    "/purchase-orders/{id}/reject",
                    web::post().to(reject_purchase_order),
                )
                .route(
                    "/purchase-orders/{id}/order",
                    web::post().to(order_purchase_order),
                )
                .route(
                    "/purchase-orders/{id}/receive",
                    web::post().to(receive_purchase_order),
                )
                .route(
                    "/purchase-orders/{id}/close",
                    web::post().to(close_purchase_order),
                )
                .route(
                    "/purchase-orders/{id}/cancel",
                    web::post().to(cancel_purchase_order),
                )
                .route(
                    "/purchase-orders/{id}/approvals",
                    web::get().to(list_po_approvals),
                )
                .route(
                    "/purchase-orders/{id}/receipts",
                    web::get().to(list_po_receipts),
                )
                .route("/purchase-orders/{id}/lines", web::get().to(list_po_lines))
                .route("/purchase-orders/{id}/lines", web::post().to(add_po_line))
                .route(
                    "/purchase-orders/{id}/lines/{line_id}",
                    web::patch().to(update_po_line),
                )
                .route(
                    "/purchase-orders/{id}/lines/{line_id}",
                    web::delete().to(delete_po_line),
                )
                .route("/receipts", web::get().to(list_receipts))
                .route("/budgets", web::get().to(list_budgets))
                .route("/budgets", web::post().to(create_budget))
                .route("/budgets/{id}", web::get().to(get_budget))
                .route("/budgets/{id}", web::patch().to(update_budget))
                .route("/budgets/{id}", web::delete().to(delete_budget))
                .route("/spend-summary", web::get().to(spend_summary)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/procurement/vendors", owner: "procurement", description: "List vendors (paginated, filter by status, category, search)." },
            RouteInfo { method: "POST", path: "/api/v1/procurement/vendors", owner: "procurement", description: "Create a vendor (payment_reference is a reference only; never raw bank secrets)." },
            RouteInfo { method: "GET", path: "/api/v1/procurement/vendors/{id}", owner: "procurement", description: "Get one vendor." },
            RouteInfo { method: "PATCH", path: "/api/v1/procurement/vendors/{id}", owner: "procurement", description: "Update a vendor." },
            RouteInfo { method: "DELETE", path: "/api/v1/procurement/vendors/{id}", owner: "procurement", description: "Delete a vendor with no purchase orders." },
            RouteInfo { method: "GET", path: "/api/v1/procurement/vendors/{id}/performance", owner: "procurement", description: "Vendor performance: on-time delivery rate, committed spend, quality notes." },
            RouteInfo { method: "GET", path: "/api/v1/procurement/purchase-orders", owner: "procurement", description: "List purchase orders (paginated, filter by status, vendor_id, category, search)." },
            RouteInfo { method: "POST", path: "/api/v1/procurement/purchase-orders", owner: "procurement", description: "Create a purchase order (draft; totals computed server-side from lines)." },
            RouteInfo { method: "GET", path: "/api/v1/procurement/purchase-orders/{id}", owner: "procurement", description: "Get one purchase order with lines and budget warnings." },
            RouteInfo { method: "PATCH", path: "/api/v1/procurement/purchase-orders/{id}", owner: "procurement", description: "Update a draft purchase order only." },
            RouteInfo { method: "DELETE", path: "/api/v1/procurement/purchase-orders/{id}", owner: "procurement", description: "Delete a draft purchase order only." },
            RouteInfo { method: "POST", path: "/api/v1/procurement/purchase-orders/{id}/submit", owner: "procurement", description: "Submit a draft purchase order for approval (draft -> pending_approval)." },
            RouteInfo { method: "POST", path: "/api/v1/procurement/purchase-orders/{id}/approve", owner: "procurement", description: "Approve a purchase order (approval row recorded; over-budget warning is advisory only)." },
            RouteInfo { method: "POST", path: "/api/v1/procurement/purchase-orders/{id}/reject", owner: "procurement", description: "Reject a purchase order with a reason (returns it to draft; rejection recorded)." },
            RouteInfo { method: "POST", path: "/api/v1/procurement/purchase-orders/{id}/order", owner: "procurement", description: "Place an approved purchase order with the vendor (approved -> ordered)." },
            RouteInfo { method: "POST", path: "/api/v1/procurement/purchase-orders/{id}/receive", owner: "procurement", description: "Receive goods against a PO line (partial receipt supported; over-receipt rejected)." },
            RouteInfo { method: "POST", path: "/api/v1/procurement/purchase-orders/{id}/close", owner: "procurement", description: "Close a received or partially received purchase order." },
            RouteInfo { method: "POST", path: "/api/v1/procurement/purchase-orders/{id}/cancel", owner: "procurement", description: "Cancel a draft, pending-approval, or approved purchase order." },
            RouteInfo { method: "GET", path: "/api/v1/procurement/purchase-orders/{id}/approvals", owner: "procurement", description: "Approval history of a purchase order (paginated, filter by decision)." },
            RouteInfo { method: "GET", path: "/api/v1/procurement/purchase-orders/{id}/receipts", owner: "procurement", description: "Goods receipts of a purchase order (paginated)." },
            RouteInfo { method: "GET", path: "/api/v1/procurement/purchase-orders/{id}/lines", owner: "procurement", description: "List lines of a purchase order (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/procurement/purchase-orders/{id}/lines", owner: "procurement", description: "Add a line to a draft purchase order (totals recomputed)." },
            RouteInfo { method: "PATCH", path: "/api/v1/procurement/purchase-orders/{id}/lines/{line_id}", owner: "procurement", description: "Update a line of a draft purchase order (totals recomputed)." },
            RouteInfo { method: "DELETE", path: "/api/v1/procurement/purchase-orders/{id}/lines/{line_id}", owner: "procurement", description: "Remove a line from a draft purchase order (totals recomputed)." },
            RouteInfo { method: "GET", path: "/api/v1/procurement/receipts", owner: "procurement", description: "List all goods receipts (paginated, filter by purchase_order_id, vendor_id)." },
            RouteInfo { method: "GET", path: "/api/v1/procurement/budgets", owner: "procurement", description: "List budgets with spend vs budget on read (filter by category, vendor_id, over_budget)." },
            RouteInfo { method: "POST", path: "/api/v1/procurement/budgets", owner: "procurement", description: "Create a budget scoped to a category, a vendor, or both." },
            RouteInfo { method: "GET", path: "/api/v1/procurement/budgets/{id}", owner: "procurement", description: "Get one budget with spend vs budget on read." },
            RouteInfo { method: "PATCH", path: "/api/v1/procurement/budgets/{id}", owner: "procurement", description: "Update a budget." },
            RouteInfo { method: "DELETE", path: "/api/v1/procurement/budgets/{id}", owner: "procurement", description: "Delete a budget." },
            RouteInfo { method: "GET", path: "/api/v1/procurement/spend-summary", owner: "procurement", description: "Committed spend totals with per-category / per-vendor breakdowns and budget comparison (filter by from, to, category, vendor_id)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ProcurementModule::new()));
}
