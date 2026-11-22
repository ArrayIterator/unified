// Point of Sale (POS) module — in-person sales at a physical till.
//
// Registers (terminals) with open/close shifts and cashier
// assignment, an in-person sale flow (cart lines resolved from the
// catalogue by product id, barcode, or SKU), configurable payment
// methods with split payment and change calculation, structured
// receipts with a printable line format, refunds and voids, and
// end-of-shift / end-of-day reports.
//
// Interconnection happens only through the hook bus; this module
// never calls another module directly:
//
//   - Sale completion emits `order.created` carrying a
//     store-shaped order payload, so the store module records the
//     order, the billing module builds its invoice from it, and the
//     tax module computes and stores the tax for it (the tax
//     module listens to `order.created`). It also emits
//     `pos.sale.completed` with the full sale and receipt id.
//   - Stock is never decremented here. Completion emits one
//     `assets.stock.decrement_requested` event per line
//     (`{asset_id, quantity, reason, sale_id, register_id,
//     product_id, sku, barcode}`); the inventory side maps the
//     catalogue product to its stock item and applies the move.
//     Refunds emit `assets.stock.increment_requested` the same way.
//   - Refunds emit `payment.refunded` (`{order_id, sale_id,
//     amount, method}`) so the store and billing modules apply
//     their own refund handling (licence suspension, credit notes).
//   - Product lookup emits `pos.product.search_requested` and
//     returns whatever catalogue handler answers; when no handler
//     answers the endpoint says so honestly instead of inventing
//     products.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory registers, shifts, sales, payments, receipts, refunds,
// and payment-method definitions; shift cash reconciliation with a
// variance report; split payments with change; receipt data plus a
// structured print-line format. What is NOT built here:
//   - True offline queueing / sync. There is no offline client in
//     this codebase and this module does not pretend one exists:
//     every sale is recorded server-side at completion time. The
//     settings response reports
//     `offline_queueing: "not_supported"` and offline support is a
//     FUTURE item for a client that can queue and replay sales.
//   - A request/response tax-quote hook. The tax module has no
//     `tax.compute.requested` listener (verified: it listens to
//     `invoice.created`, `order.created`, and
//     `tax.filing.check_requested` only), so POS computes line tax
//     from the caller-supplied per-line tax rate (resolved by the
//     terminal from the tax rules, or the configured default rate)
//     and the tax module independently computes and stores the tax
//     from the emitted `order.created` payload. The POS tax figures
//     are a snapshot; the tax module's computation is authoritative.
//   - Stock application. The decrement/increment events above are a
//     defined contract, but the assets module currently registers
//     no listener for them (verified: its only listener is
//     `assets.warranty.check_requested`), so until the inventory
//     side answers them, stock does not move. The follow-up is to
//     add the listener on the assets side.
//   - Persistence. Durable rows are defined by migration
//     `2026-11-22-000074_pos`; until that migration is executed,
//     POS records do not survive a restart. Payment-method
//     definitions and POS settings are admin-editable runtime
//     configuration (same pattern as the billing settings); their
//     durable form is a follow-up for the settings store.
//
// There are no public (unauthenticated) routes in this module:
// every route requires the `X-User-Id` / `X-Role` headers and a
// capability check, and no POS route is exposed through the API
// Manager public-access rules (default deny).
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the assets module. Capability checks use
// the `role.check` hook and fail closed (no answer means not
// allowed).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::HookBus;
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_NAME_LEN: usize = 200;
const MAX_CODE_LEN: usize = 64;
const MAX_LOCATION_LEN: usize = 200;
const MAX_NOTES_LEN: usize = 5000;
const MAX_REASON_LEN: usize = 1000;
const MAX_LABEL_LEN: usize = 120;
/// Default receipt width in characters. Configurable in settings.
const DEFAULT_RECEIPT_WIDTH: u32 = 42;
/// Maximum receipt width a caller may configure.
const MAX_RECEIPT_WIDTH: u32 = 80;
/// Maximum discount percent a sale or line may carry (configurable
/// in settings as `max_discount_percent`; this is the ceiling for
/// the setting itself).
const MAX_DISCOUNT_PERCENT: f64 = 100.0;
/// Tolerance when comparing money sums (rounding to cents).
const MONEY_EPSILON: f64 = 0.01;

/// Capability names checked through the `role.check` hook.
const PERM_POS_VIEW: &str = "pos.view";
const PERM_POS_SELL: &str = "pos.sell";
const PERM_POS_MANAGE: &str = "pos.manage";
const PERM_POS_REFUND: &str = "pos.refund";

/// Events this module emits.
const EVENT_SALE_COMPLETED: &str = "pos.sale.completed";
const EVENT_STOCK_DECREMENT_REQUESTED: &str = "assets.stock.decrement_requested";
const EVENT_STOCK_INCREMENT_REQUESTED: &str = "assets.stock.increment_requested";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One POS terminal / register. `cashiers` holds the user ids
/// allowed to open shifts and sell on this register; a manager
/// (holding `pos.manage`) may also sell without being listed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PosRegister {
    pub id: String,
    pub name: String,
    /// Short code, unique across registers (for example `TILL-01`).
    pub code: String,
    #[serde(default)]
    pub location: Option<String>,
    /// `active` or `inactive`. Inactive registers cannot open shifts.
    pub status: String,
    /// Assigned cashier user ids.
    #[serde(default)]
    pub cashiers: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One cashier shift on a register: `open` until closed with a
/// counted cash drawer. `expected_cash` and `variance` are computed
/// at close time from the opening float, the cash payments of the
/// completed sales in the shift, and the cash refunds tied to it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PosShift {
    pub id: String,
    pub register_id: String,
    pub cashier_id: String,
    /// `open` or `closed`.
    pub status: String,
    pub opening_float: f64,
    pub currency: String,
    /// Cash the drawer should hold at close (computed at close).
    #[serde(default)]
    pub expected_cash: Option<f64>,
    /// Cash actually counted at close.
    #[serde(default)]
    pub counted_cash: Option<f64>,
    /// `counted_cash - expected_cash` (computed at close).
    #[serde(default)]
    pub variance: Option<f64>,
    #[serde(default)]
    pub notes: Option<String>,
    pub opened_at: DateTime<Utc>,
    #[serde(default)]
    pub closed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One line of a sale. `tax_rate` is a fraction (0.10 means 10%)
/// resolved by the terminal from the tax rules (or the configured
/// default rate); the tax module independently computes the tax
/// from the emitted `order.created` payload (see the module docs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PosSaleItem {
    pub id: String,
    pub product_id: String,
    pub name: String,
    #[serde(default)]
    pub barcode: Option<String>,
    #[serde(default)]
    pub sku: Option<String>,
    pub unit_price: f64,
    pub quantity: u32,
    /// Quantity already refunded on this line.
    #[serde(default)]
    pub refunded_quantity: u32,
    /// `percent` or `fixed` (empty means no line discount).
    #[serde(default)]
    pub discount_type: Option<String>,
    /// Percent (0..=100) or fixed amount in the sale currency.
    #[serde(default)]
    pub discount_value: f64,
    /// Discount amount for the whole line, in major units.
    #[serde(default)]
    pub discount_amount: f64,
    /// Net line amount after discounts, before tax.
    #[serde(default)]
    pub line_subtotal: f64,
    /// Tax rate applied to this line, as a fraction.
    #[serde(default)]
    pub tax_rate: f64,
    /// Tax amount for this line.
    #[serde(default)]
    pub tax_amount: f64,
    /// Line total including tax.
    #[serde(default)]
    pub line_total: f64,
}

/// One payment applied to a sale. Split payment is normal: a sale
/// may carry several payments whose amounts sum to the sale total.
/// `tendered` is the cash handed over (cash methods only); the
/// change due is `tendered - amount` summed across payments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PosPayment {
    pub id: String,
    pub sale_id: String,
    pub method_id: String,
    pub method_code: String,
    pub amount: f64,
    /// Amount tendered by the customer; defaults to `amount`.
    #[serde(default)]
    pub tendered: f64,
    #[serde(default)]
    pub reference: Option<String>,
    pub recorded_by: String,
    pub recorded_at: DateTime<Utc>,
}

/// One in-person sale. Statuses: `draft` (open cart), `held`
/// (parked for later), `completed` (paid), `voided` (cancelled
/// before payment), `partially_refunded`, `refunded`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PosSale {
    pub id: String,
    /// Human-readable sale number, for example `POS-000001`.
    pub number: String,
    pub register_id: String,
    #[serde(default)]
    pub shift_id: Option<String>,
    pub cashier_id: String,
    #[serde(default)]
    pub company_id: Option<String>,
    pub items: Vec<PosSaleItem>,
    /// Net amount: sum of the net line amounts after line discounts.
    #[serde(default)]
    pub subtotal: f64,
    /// Sale-level discount type: `percent`, `fixed`, or empty.
    #[serde(default)]
    pub discount_type: Option<String>,
    /// Sale-level discount value (percent or fixed amount).
    #[serde(default)]
    pub discount_value: f64,
    /// Sale-level discount amount, in major units.
    #[serde(default)]
    pub discount_total: f64,
    /// Total tax across all lines.
    #[serde(default)]
    pub tax_total: f64,
    /// Total amount due, including tax.
    #[serde(default)]
    pub total: f64,
    pub currency: String,
    /// Whether line prices include tax.
    #[serde(default)]
    pub tax_inclusive: bool,
    /// `draft`, `held`, `completed`, `voided`,
    /// `partially_refunded`, or `refunded`.
    pub status: String,
    /// Amount paid so far across all recorded payments.
    #[serde(default)]
    pub amount_paid: f64,
    /// Change handed back to the customer at completion.
    #[serde(default)]
    pub change_due: f64,
    /// Amount refunded so far across all refunds.
    #[serde(default)]
    pub refunded_amount: f64,
    #[serde(default)]
    pub customer_name: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub voided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One refund of a completed sale: by line items (restock events
/// are emitted per returned line) or by a plain amount.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PosRefund {
    pub id: String,
    pub sale_id: String,
    /// Shift the sale belonged to, for drawer reconciliation.
    #[serde(default)]
    pub shift_id: Option<String>,
    pub amount: f64,
    pub method_id: String,
    pub method_code: String,
    #[serde(default)]
    pub items: Vec<PosRefundItem>,
    pub reason: String,
    pub refunded_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PosRefundItem {
    pub item_id: String,
    pub product_id: String,
    pub quantity: u32,
    pub amount: f64,
}

/// One line of a printable receipt: structured data for a receipt
/// printer, not a print driver. `kind` is one of `header`,
/// `text`, `item`, `divider`, `total`, `payment`, or `footer`;
/// `align` is `left`, `center`, or `right`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptPrintLine {
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub align: Option<String>,
}

/// The receipt for a completed sale: the full receipt data plus
/// the structured print lines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PosReceipt {
    pub id: String,
    /// Human-readable receipt number, for example `RCP-000001`.
    pub number: String,
    pub sale_id: String,
    pub sale_number: String,
    pub register_id: String,
    #[serde(default)]
    pub register_code: Option<String>,
    pub cashier_id: String,
    pub store_name: String,
    #[serde(default)]
    pub store_address: Option<String>,
    pub items: Vec<PosSaleItem>,
    pub subtotal: f64,
    pub discount_total: f64,
    pub tax_total: f64,
    pub total: f64,
    pub currency: String,
    /// Tax grouped by rate, for the receipt tax breakdown.
    #[serde(default)]
    pub tax_breakdown: Vec<TaxBreakdownLine>,
    pub payments: Vec<PosPayment>,
    pub amount_paid: f64,
    pub change_due: f64,
    pub print_lines: Vec<ReceiptPrintLine>,
    #[serde(default)]
    pub footer: Option<String>,
    pub issued_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxBreakdownLine {
    pub tax_rate: f64,
    pub tax_amount: f64,
}

/// One payment method definition. The list is admin-editable
/// runtime configuration, never a hard-coded list in the sale
/// flow: a payment is valid only when its method id resolves to
/// an enabled definition here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PosPaymentMethod {
    pub id: String,
    /// Short code, unique across methods (for example `cash`).
    pub code: String,
    pub label: String,
    /// Free-form kind label (for example `cash`, `card`,
    /// `transfer`, `wallet`). Admin-defined; the sale flow does
    /// not branch on any particular kind value except through
    /// `is_cash`, which marks methods that live in the drawer.
    #[serde(default)]
    pub kind: String,
    /// True when payments with this method live in the cash
    /// drawer and count towards the shift cash reconciliation.
    #[serde(default)]
    pub is_cash: bool,
    /// True when a payment with this method must carry a
    /// reference (card authorisation code, transfer reference).
    #[serde(default)]
    pub requires_reference: bool,
    /// `true` means the method can be used on new sales.
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// POS configuration. All values are admin-editable through the
/// settings endpoints; the values below are configurable defaults,
/// not hard-coded behaviour.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PosSettings {
    pub store_name: String,
    #[serde(default)]
    pub store_address: Option<String>,
    #[serde(default)]
    pub receipt_footer: Option<String>,
    /// Receipt width in characters for the print lines.
    pub receipt_width_chars: u32,
    pub default_currency: String,
    /// Default per-line tax rate as a fraction (0.10 means 10%).
    pub default_tax_rate: f64,
    /// Whether line prices include tax by default.
    pub tax_inclusive: bool,
    /// Maximum discount percent a line or sale may carry.
    pub max_discount_percent: f64,
    /// When true, only cashiers assigned to a register may open
    /// shifts and sell on it (managers holding `pos.manage` are
    /// always allowed).
    pub require_cashier_assignment: bool,
    /// When false, a sale accepts exactly one payment.
    pub allow_split_payment: bool,
}

impl Default for PosSettings {
    fn default() -> Self {
        Self {
            store_name: "Store".to_string(),
            store_address: None,
            receipt_footer: Some("Thank you for shopping with us.".to_string()),
            receipt_width_chars: DEFAULT_RECEIPT_WIDTH,
            default_currency: "USD".to_string(),
            default_tax_rate: 0.0,
            tax_inclusive: false,
            max_discount_percent: 100.0,
            require_cashier_assignment: true,
            allow_split_payment: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateRegisterRequest {
    pub name: String,
    pub code: Option<String>,
    pub location: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRegisterRequest {
    pub name: Option<String>,
    pub location: Option<Option<String>>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AssignCashierRequest {
    pub user_id: String,
}

#[derive(Debug, Deserialize)]
pub struct OpenShiftRequest {
    pub register_id: String,
    pub opening_float: Option<f64>,
    pub currency: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CloseShiftRequest {
    pub counted_cash: f64,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct NewSaleItemRequest {
    pub product_id: String,
    pub name: String,
    pub barcode: Option<String>,
    pub sku: Option<String>,
    pub unit_price: f64,
    pub quantity: Option<u32>,
    pub discount_type: Option<String>,
    pub discount_value: Option<f64>,
    pub tax_rate: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSaleRequest {
    pub register_id: String,
    pub shift_id: Option<String>,
    pub company_id: Option<String>,
    pub currency: Option<String>,
    pub customer_name: Option<String>,
    pub notes: Option<String>,
    pub discount_type: Option<String>,
    pub discount_value: Option<f64>,
    pub items: Option<Vec<NewSaleItemRequest>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSaleRequest {
    pub discount_type: Option<Option<String>>,
    pub discount_value: Option<f64>,
    pub customer_name: Option<Option<String>>,
    pub notes: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSaleItemRequest {
    pub quantity: Option<u32>,
    pub unit_price: Option<f64>,
    pub discount_type: Option<Option<String>>,
    pub discount_value: Option<f64>,
    pub tax_rate: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct CompletePaymentRequest {
    pub method_id: String,
    pub amount: f64,
    pub tendered: Option<f64>,
    pub reference: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CompleteSaleRequest {
    pub payments: Vec<CompletePaymentRequest>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RefundItemRequest {
    pub item_id: String,
    pub quantity: u32,
}

#[derive(Debug, Deserialize)]
pub struct RefundSaleRequest {
    pub items: Option<Vec<RefundItemRequest>>,
    pub amount: Option<f64>,
    pub method_id: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePaymentMethodRequest {
    pub code: String,
    pub label: String,
    pub kind: Option<String>,
    pub is_cash: Option<bool>,
    pub requires_reference: Option<bool>,
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePaymentMethodRequest {
    pub label: Option<String>,
    pub kind: Option<String>,
    pub is_cash: Option<bool>,
    pub requires_reference: Option<bool>,
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    pub store_name: Option<String>,
    pub store_address: Option<Option<String>>,
    pub receipt_footer: Option<Option<String>>,
    pub receipt_width_chars: Option<u32>,
    pub default_currency: Option<String>,
    pub default_tax_rate: Option<f64>,
    pub tax_inclusive: Option<bool>,
    pub max_discount_percent: Option<f64>,
    pub require_cashier_assignment: Option<bool>,
    pub allow_split_payment: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ProductSearchQuery {
    pub q: Option<String>,
    pub barcode: Option<String>,
    pub sku: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct ReportQuery {
    pub date: Option<String>,
    pub register_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for POS. The hook bus is attached in `init`
/// (the bus lives in `AppContext`); handlers tolerate a missing bus
/// by skipping event emission. Durable rows for these types are
/// defined by migration `2026-11-22-000074_pos`; until that
/// migration is executed, POS records do not survive a restart.
#[derive(Clone)]
pub struct PosState {
    pub registers: Arc<DashMap<String, PosRegister>>,
    pub shifts: Arc<DashMap<String, PosShift>>,
    pub sales: Arc<DashMap<String, PosSale>>,
    pub payments: Arc<DashMap<String, PosPayment>>,
    pub receipts: Arc<DashMap<String, PosReceipt>>,
    pub refunds: Arc<DashMap<String, PosRefund>>,
    pub payment_methods: Arc<DashMap<String, PosPaymentMethod>>,
    pub settings: Arc<RwLock<PosSettings>>,
    pub sale_seq: Arc<AtomicU64>,
    pub receipt_seq: Arc<AtomicU64>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for PosState {
    fn default() -> Self {
        Self {
            registers: Arc::new(DashMap::new()),
            shifts: Arc::new(DashMap::new()),
            sales: Arc::new(DashMap::new()),
            payments: Arc::new(DashMap::new()),
            receipts: Arc::new(DashMap::new()),
            refunds: Arc::new(DashMap::new()),
            payment_methods: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(PosSettings::default())),
            sale_seq: Arc::new(AtomicU64::new(1)),
            receipt_seq: Arc::new(AtomicU64::new(1)),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl PosState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    fn settings(&self) -> PosSettings {
        self.settings
            .read()
            .expect("pos settings lock poisoned")
            .clone()
    }

    fn next_sale_number(&self) -> String {
        let seq = self.sale_seq.fetch_add(1, Ordering::SeqCst);
        format!("POS-{seq:06}")
    }

    fn next_receipt_number(&self) -> String {
        let seq = self.receipt_seq.fetch_add(1, Ordering::SeqCst);
        format!("RCP-{seq:06}")
    }

    /// Seed the payment-method definitions with configurable
    /// defaults on first use. These are starting values an admin
    /// edits or deletes through the payment-method endpoints; the
    /// sale flow never assumes any of them exists and validates
    /// every payment against the stored definitions.
    fn seed_payment_methods(&self) {
        if !self.payment_methods.is_empty() {
            return;
        }
        let created = now();
        let defaults: [(&str, &str, &str, bool, bool); 4] = [
            ("cash", "Cash", "cash", true, false),
            ("card", "Card", "card", false, true),
            ("bank_transfer", "Bank Transfer", "transfer", false, true),
            ("e_wallet", "E-Wallet", "wallet", false, true),
        ];
        for (code, label, kind, is_cash, requires_reference) in defaults {
            let method = PosPaymentMethod {
                id: new_id(),
                code: code.to_string(),
                label: label.to_string(),
                kind: kind.to_string(),
                is_cash,
                requires_reference,
                active: true,
                created_at: created,
                updated_at: created,
            };
            self.payment_methods.insert(method.id.clone(), method);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn round_money(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the assets module.
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
/// (same fail-closed shape as the assets module's capability check).
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

fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            if trimmed.len() > max {
                return Err(UnifiedError::bad_request(format!(
                    "{field} must be at most {max} characters"
                )));
            }
            Ok(Some(trimmed.to_string()))
        }
    }
}

/// Money: finite, non-negative, rounded to cents.
fn valid_money(amount: f64, field: &str) -> UnifiedResult<f64> {
    if !amount.is_finite() || amount < 0.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a finite number zero or greater"
        )));
    }
    Ok(round_money(amount))
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

/// Discount type: empty (none), `percent`, or `fixed`.
fn valid_discount_type(value: &Option<String>) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(raw) => {
            let trimmed = raw.trim().to_lowercase();
            if trimmed.is_empty() {
                return Ok(None);
            }
            if !matches!(trimmed.as_str(), "percent" | "fixed") {
                return Err(UnifiedError::bad_request(
                    "discount_type must be one of: percent, fixed",
                ));
            }
            Ok(Some(trimmed))
        }
    }
}

/// Discount value for a percent/fixed discount against a base
/// amount. Percent is capped at the configured maximum; a fixed
/// discount never exceeds the base amount.
fn discount_amount(
    discount_type: &Option<String>,
    discount_value: f64,
    base: f64,
    max_percent: f64,
) -> UnifiedResult<f64> {
    match discount_type.as_deref() {
        Some("percent") => {
            if !discount_value.is_finite() || discount_value < 0.0 || discount_value > max_percent {
                return Err(UnifiedError::bad_request(format!(
                    "discount percent must be between 0 and {max_percent}"
                )));
            }
            Ok(round_money(base * discount_value / 100.0).min(base))
        }
        Some("fixed") => {
            if !discount_value.is_finite() || discount_value < 0.0 {
                return Err(UnifiedError::bad_request(
                    "discount amount must be zero or greater",
                ));
            }
            Ok(round_money(discount_value).min(base))
        }
        _ => Ok(0.0),
    }
}

/// Tax on a net amount for a rate given as a fraction. When tax is
/// inclusive the amount already contains the tax; otherwise the tax
/// is added on top. Returns `(net, tax)`.
fn tax_on_amount(amount: f64, tax_rate: f64, tax_inclusive: bool) -> (f64, f64) {
    let amount = round_money(amount);
    if tax_inclusive {
        let net = round_money(amount / (1.0 + tax_rate));
        (net, round_money(amount - net))
    } else {
        (amount, round_money(amount * tax_rate))
    }
}

/// Recompute every derived total of a sale from its lines and its
/// sale-level discount. The sale discount is spread across the
/// lines in proportion to their net amounts; tax is then computed
/// per line on the discounted base, and every total is rounded to
/// cents.
fn recompute_sale(sale: &mut PosSale, settings: &PosSettings) {
    let gross_net: f64 = sale
        .items
        .iter()
        .map(|item| round_money(item.unit_price * item.quantity as f64) - item.discount_amount)
        .map(|net| net.max(0.0))
        .sum();
    let sale_discount = discount_amount(
        &sale.discount_type,
        sale.discount_value,
        gross_net,
        settings.max_discount_percent,
    )
    .unwrap_or(0.0);
    let ratio = if gross_net > 0.0 {
        (sale_discount / gross_net).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let mut subtotal = 0.0;
    let mut tax_total = 0.0;
    for item in sale.items.iter_mut() {
        let net = (round_money(item.unit_price * item.quantity as f64) - item.discount_amount)
            .max(0.0);
        let discounted_base = round_money(net * (1.0 - ratio));
        let (line_net, tax) = tax_on_amount(discounted_base, item.tax_rate, sale.tax_inclusive);
        item.line_subtotal = line_net;
        item.tax_amount = tax;
        item.line_total = round_money(line_net + tax);
        subtotal += line_net;
        tax_total += tax;
    }
    sale.subtotal = round_money(subtotal);
    sale.discount_total = round_money(sale_discount);
    sale.tax_total = round_money(tax_total);
    sale.total = round_money(subtotal + tax_total);
}

/// Build one sale line from a request, validating price, quantity,
/// discount, and tax rate.
fn build_sale_item(
    input: &NewSaleItemRequest,
    settings: &PosSettings,
) -> UnifiedResult<PosSaleItem> {
    let product_id = require_text(&input.product_id, "product_id", MAX_CODE_LEN)?;
    let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
    let unit_price = valid_money(input.unit_price, "unit_price")?;
    let quantity = input.quantity.unwrap_or(1);
    if quantity == 0 || quantity > 10_000 {
        return Err(UnifiedError::bad_request(
            "quantity must be between 1 and 10000",
        ));
    }
    let discount_type = valid_discount_type(&input.discount_type)?;
    let discount_value = input.discount_value.unwrap_or(0.0);
    let gross = round_money(unit_price * quantity as f64);
    let discount_amount = discount_amount(
        &discount_type,
        discount_value,
        gross,
        settings.max_discount_percent,
    )?;
    let tax_rate = input.tax_rate.unwrap_or(settings.default_tax_rate);
    if !tax_rate.is_finite() || tax_rate < 0.0 || tax_rate > 10.0 {
        return Err(UnifiedError::bad_request(
            "tax_rate must be a finite fraction between 0 and 10",
        ));
    }
    let barcode = optional_text(&input.barcode, "barcode", MAX_CODE_LEN)?;
    let sku = optional_text(&input.sku, "sku", MAX_CODE_LEN)?;
    Ok(PosSaleItem {
        id: new_id(),
        product_id,
        name,
        barcode,
        sku,
        unit_price,
        quantity,
        refunded_quantity: 0,
        discount_type,
        discount_value,
        discount_amount,
        line_subtotal: 0.0,
        tax_rate,
        tax_amount: 0.0,
        line_total: 0.0,
    })
}

fn load_register(state: &PosState, id: &str) -> UnifiedResult<PosRegister> {
    state
        .registers
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Register not found: {id}")))
}

fn load_shift(state: &PosState, id: &str) -> UnifiedResult<PosShift> {
    state
        .shifts
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Shift not found: {id}")))
}

fn load_sale(state: &PosState, id: &str) -> UnifiedResult<PosSale> {
    state
        .sales
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Sale not found: {id}")))
}

fn open_shift_for_register(state: &PosState, register_id: &str) -> Option<PosShift> {
    state
        .shifts
        .iter()
        .map(|entry| entry.value().clone())
        .find(|shift| shift.register_id == register_id && shift.status == "open")
}

/// True when the caller may sell on this register: a manager, or
/// an assigned cashier when assignment is required.
fn caller_may_sell(
    ctx: &AppContext,
    state: &PosState,
    register: &PosRegister,
    caller: &Caller,
) -> UnifiedResult<()> {
    if check_capability(ctx, &caller.role, PERM_POS_MANAGE) {
        return Ok(());
    }
    let settings = state.settings();
    if !settings.require_cashier_assignment {
        return Ok(());
    }
    if register.cashiers.iter().any(|id| id == &caller.actor) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Caller is not an assigned cashier for this register",
        ))
    }
}

/// True when the caller may act on this sale: the sale cashier, or
/// a manager holding `pos.manage`.
fn caller_may_act_on_sale(
    ctx: &AppContext,
    sale: &PosSale,
    caller: &Caller,
) -> UnifiedResult<()> {
    if caller.actor == sale.cashier_id {
        return Ok(());
    }
    if check_capability(ctx, &caller.role, PERM_POS_MANAGE) {
        return Ok(());
    }
    Err(UnifiedError::forbidden(
        "Caller is not the cashier of this sale",
    ))
}

/// Cash the drawer should hold for a shift: opening float plus the
/// cash payments of the completed sales in the shift, minus the
/// cash refunds tied to the shift.
fn expected_cash_for_shift(state: &PosState, shift: &PosShift) -> f64 {
    let cash_method_ids: Vec<String> = state
        .payment_methods
        .iter()
        .filter(|entry| entry.value().is_cash)
        .map(|entry| entry.key().clone())
        .collect();
    let mut expected = shift.opening_float;
    for entry in state.sales.iter() {
        let sale = entry.value();
        if sale.shift_id.as_deref() != Some(shift.id.as_str()) {
            continue;
        }
        if sale.status != "completed"
            && sale.status != "partially_refunded"
            && sale.status != "refunded"
        {
            continue;
        }
        for payment in state.payments.iter() {
            let payment = payment.value();
            if payment.sale_id == sale.id && cash_method_ids.contains(&payment.method_id) {
                expected += payment.amount;
            }
        }
    }
    for entry in state.refunds.iter() {
        let refund = entry.value();
        if refund.shift_id.as_deref() == Some(shift.id.as_str())
            && cash_method_ids.contains(&refund.method_id)
        {
            expected -= refund.amount;
        }
    }
    round_money(expected)
}

/// Totals of the completed sales in a shift, grouped by payment
/// method code, plus refund and void counts.
fn shift_summary(state: &PosState, shift: &PosShift) -> Value {
    let mut by_method: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    let mut sales_count = 0u64;
    let mut gross = 0.0;
    for entry in state.sales.iter() {
        let sale = entry.value();
        if sale.shift_id.as_deref() != Some(shift.id.as_str()) {
            continue;
        }
        if sale.status == "voided" {
            continue;
        }
        if sale.status != "completed"
            && sale.status != "partially_refunded"
            && sale.status != "refunded"
        {
            continue;
        }
        sales_count += 1;
        gross += sale.total;
        for payment in state.payments.iter() {
            let payment = payment.value();
            if payment.sale_id == sale.id {
                *by_method.entry(payment.method_code.clone()).or_insert(0.0) += payment.amount;
            }
        }
    }
    let mut refunds_total = 0.0;
    let mut refunds_count = 0u64;
    for entry in state.refunds.iter() {
        let refund = entry.value();
        if refund.shift_id.as_deref() == Some(shift.id.as_str()) {
            refunds_total += refund.amount;
            refunds_count += 1;
        }
    }
    let voids_count = state
        .sales
        .iter()
        .filter(|entry| {
            entry.value().shift_id.as_deref() == Some(shift.id.as_str())
                && entry.value().status == "voided"
        })
        .count() as u64;
    let mut by_method_json: Vec<Value> = by_method
        .into_iter()
        .map(|(method, amount)| {
            json!({ "method": method, "amount": round_money(amount) })
        })
        .collect();
    by_method_json.sort_by(|a, b| {
        a.get("method")
            .and_then(Value::as_str)
            .cmp(&b.get("method").and_then(Value::as_str))
    });
    json!({
        "shift_id": shift.id,
        "register_id": shift.register_id,
        "cashier_id": shift.cashier_id,
        "status": shift.status,
        "sales_count": sales_count,
        "gross_sales": round_money(gross),
        "refunds_total": round_money(refunds_total),
        "refunds_count": refunds_count,
        "voids_count": voids_count,
        "net_sales": round_money(gross - refunds_total),
        "by_payment_method": by_method_json,
        "opening_float": shift.opening_float,
        "expected_cash": shift.expected_cash,
        "counted_cash": shift.counted_cash,
        "variance": shift.variance,
        "currency": shift.currency,
        "opened_at": shift.opened_at,
        "closed_at": shift.closed_at,
    })
}

/// Build the structured print lines for a receipt at the
/// configured width. Plain text lines a receipt printer renders
/// in order; this is receipt data for printing, not a driver.
fn build_print_lines(
    receipt: &PosReceipt,
    settings: &PosSettings,
) -> Vec<ReceiptPrintLine> {
    let width = settings.receipt_width_chars.max(16) as usize;
    let mut lines: Vec<ReceiptPrintLine> = Vec::new();
    let center = |text: &str| ReceiptPrintLine {
        kind: "header".to_string(),
        text: text.to_string(),
        bold: true,
        align: Some("center".to_string()),
    };
    lines.push(center(&receipt.store_name));
    if let Some(address) = &receipt.store_address {
        lines.push(ReceiptPrintLine {
            kind: "text".to_string(),
            text: address.clone(),
            bold: false,
            align: Some("center".to_string()),
        });
    }
    lines.push(ReceiptPrintLine {
        kind: "divider".to_string(),
        text: "-".repeat(width),
        bold: false,
        align: None,
    });
    lines.push(ReceiptPrintLine {
        kind: "text".to_string(),
        text: format!("Receipt {}", receipt.number),
        bold: false,
        align: None,
    });
    lines.push(ReceiptPrintLine {
        kind: "text".to_string(),
        text: format!("Sale {}", receipt.sale_number),
        bold: false,
        align: None,
    });
    lines.push(ReceiptPrintLine {
        kind: "text".to_string(),
        text: format!(
            "Register {}   Cashier {}",
            receipt.register_code.clone().unwrap_or_default(),
            receipt.cashier_id
        ),
        bold: false,
        align: None,
    });
    lines.push(ReceiptPrintLine {
        kind: "text".to_string(),
        text: receipt.issued_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        bold: false,
        align: None,
    });
    lines.push(ReceiptPrintLine {
        kind: "divider".to_string(),
        text: "-".repeat(width),
        bold: false,
        align: None,
    });
    for item in &receipt.items {
        lines.push(ReceiptPrintLine {
            kind: "item".to_string(),
            text: format!("{} x {}", item.quantity, item.name),
            bold: false,
            align: None,
        });
        let amount = format!("{} {:.2}", receipt.currency, item.line_total);
        let left = format!("  @ {} {:.2} each", receipt.currency, item.unit_price);
        let padding = width.saturating_sub(left.len() + amount.len());
        lines.push(ReceiptPrintLine {
            kind: "item".to_string(),
            text: format!("{left}{}{amount}", " ".repeat(padding)),
            bold: false,
            align: None,
        });
        if item.discount_amount > 0.0 {
            lines.push(ReceiptPrintLine {
                kind: "text".to_string(),
                text: format!(
                    "  Discount -{} {:.2}",
                    receipt.currency, item.discount_amount
                ),
                bold: false,
                align: None,
            });
        }
    }
    lines.push(ReceiptPrintLine {
        kind: "divider".to_string(),
        text: "-".repeat(width),
        bold: false,
        align: None,
    });
    let money_line = |label: &str, amount: f64, bold: bool| ReceiptPrintLine {
        kind: if bold { "total".to_string() } else { "text".to_string() },
        text: {
            let value = format!("{} {:.2}", receipt.currency, amount);
            let padding = width.saturating_sub(label.len() + value.len());
            format!("{label}{}{value}", " ".repeat(padding))
        },
        bold,
        align: None,
    };
    lines.push(money_line("Subtotal", receipt.subtotal, false));
    if receipt.discount_total > 0.0 {
        lines.push(money_line("Discount", -receipt.discount_total, false));
    }
    for line in &receipt.tax_breakdown {
        lines.push(money_line(
            &format!("Tax {:.2}%", line.tax_rate * 100.0),
            line.tax_amount,
            false,
        ));
    }
    lines.push(money_line("TOTAL", receipt.total, true));
    for payment in &receipt.payments {
        lines.push(ReceiptPrintLine {
            kind: "payment".to_string(),
            text: {
                let label = payment.method_code.clone();
                let value = format!("{} {:.2}", receipt.currency, payment.amount);
                let padding = width.saturating_sub(label.len() + value.len());
                format!("{label}{}{value}", " ".repeat(padding))
            },
            bold: false,
            align: None,
        });
    }
    if receipt.change_due > 0.0 {
        lines.push(money_line("Change", receipt.change_due, false));
    }
    lines.push(ReceiptPrintLine {
        kind: "divider".to_string(),
        text: "-".repeat(width),
        bold: false,
        align: None,
    });
    if let Some(footer) = &receipt.footer {
        lines.push(ReceiptPrintLine {
            kind: "footer".to_string(),
            text: footer.clone(),
            bold: false,
            align: Some("center".to_string()),
        });
    }
    lines
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

/// The Point of Sale module.
pub struct PosModule {
    state: PosState,
}

impl PosModule {
    pub fn new() -> Self {
        Self {
            state: PosState::default(),
        }
    }
}

impl Default for PosModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for PosModule {
    fn id(&self) -> &'static str {
        "pos"
    }

    fn name(&self) -> &'static str {
        "Point of Sale"
    }

    fn description(&self) -> &'static str {
        "In-person sales at a physical till: registers with shifts and cashier assignment, cart sales with discounts and taxes, configurable payment methods with split payment and change, structured receipts, refunds and voids, and end-of-shift and end-of-day reports. Sales emit store orders, billing invoices, and stock movements through the hook bus."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["store", "billing", "assets", "tax"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["store", "billing", "assets", "tax"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["actix-web", "dashmap", "serde", "chrono", "uuid"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("commerce")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        self.state.seed_payment_methods();
        Ok(())
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/pos")
                .app_data(state)
                // Registers.
                .route("/registers", web::get().to(list_registers))
                .route("/registers", web::post().to(create_register))
                .route("/registers/{id}", web::get().to(get_register))
                .route("/registers/{id}", web::patch().to(update_register))
                .route("/registers/{id}", web::delete().to(delete_register))
                .route(
                    "/registers/{id}/assign-cashier",
                    web::post().to(assign_cashier),
                )
                .route(
                    "/registers/{id}/unassign-cashier",
                    web::post().to(unassign_cashier),
                )
                // Shifts.
                .route("/shifts", web::get().to(list_shifts))
                .route("/shifts/open", web::post().to(open_shift))
                .route("/shifts/{id}", web::get().to(get_shift))
                .route("/shifts/{id}/close", web::post().to(close_shift))
                .route("/shifts/{id}/summary", web::get().to(get_shift_summary))
                // Sales.
                .route("/sales", web::get().to(list_sales))
                .route("/sales", web::post().to(create_sale))
                .route("/sales/{id}", web::get().to(get_sale))
                .route("/sales/{id}", web::patch().to(update_sale))
                .route("/sales/{id}/items", web::post().to(add_sale_item))
                .route(
                    "/sales/{id}/items/{item_id}",
                    web::patch().to(update_sale_item),
                )
                .route(
                    "/sales/{id}/items/{item_id}",
                    web::delete().to(remove_sale_item),
                )
                .route("/sales/{id}/complete", web::post().to(complete_sale))
                .route("/sales/{id}/void", web::post().to(void_sale))
                .route("/sales/{id}/refund", web::post().to(refund_sale))
                .route("/sales/{id}/hold", web::post().to(hold_sale))
                .route("/sales/{id}/resume", web::post().to(resume_sale))
                // Receipts.
                .route("/receipts", web::get().to(list_receipts))
                .route("/receipts/{sale_id}", web::get().to(get_receipt))
                .route(
                    "/receipts/{sale_id}/print",
                    web::get().to(get_receipt_print),
                )
                // Catalogue lookup (hook-driven).
                .route("/products/search", web::get().to(search_products))
                // Payment methods (admin configuration).
                .route("/payment-methods", web::get().to(list_payment_methods))
                .route(
                    "/payment-methods",
                    web::post().to(create_payment_method),
                )
                .route(
                    "/payment-methods/{id}",
                    web::patch().to(update_payment_method),
                )
                .route(
                    "/payment-methods/{id}",
                    web::delete().to(delete_payment_method),
                )
                // Settings.
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                // Reports.
                .route("/reports/end-of-day", web::get().to(end_of_day_report)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/pos/registers", owner: "pos", description: "List registers (paginated, filter by status, search by name/code)." },
            RouteInfo { method: "POST", path: "/api/v1/pos/registers", owner: "pos", description: "Create a register (code must be unique when given)." },
            RouteInfo { method: "GET", path: "/api/v1/pos/registers/{id}", owner: "pos", description: "Get one register with its assigned cashiers." },
            RouteInfo { method: "PATCH", path: "/api/v1/pos/registers/{id}", owner: "pos", description: "Update a register (name, location, status)." },
            RouteInfo { method: "DELETE", path: "/api/v1/pos/registers/{id}", owner: "pos", description: "Delete a register with no open shift and no sales." },
            RouteInfo { method: "POST", path: "/api/v1/pos/registers/{id}/assign-cashier", owner: "pos", description: "Assign a cashier (user id) to a register." },
            RouteInfo { method: "POST", path: "/api/v1/pos/registers/{id}/unassign-cashier", owner: "pos", description: "Remove a cashier from a register." },
            RouteInfo { method: "GET", path: "/api/v1/pos/shifts", owner: "pos", description: "List shifts (paginated, filter by register_id, cashier_id, status)." },
            RouteInfo { method: "POST", path: "/api/v1/pos/shifts/open", owner: "pos", description: "Open a shift on a register with an opening float (one open shift per register)." },
            RouteInfo { method: "GET", path: "/api/v1/pos/shifts/{id}", owner: "pos", description: "Get one shift." },
            RouteInfo { method: "POST", path: "/api/v1/pos/shifts/{id}/close", owner: "pos", description: "Close a shift with a counted cash drawer; reports expected cash and variance." },
            RouteInfo { method: "GET", path: "/api/v1/pos/shifts/{id}/summary", owner: "pos", description: "Shift totals by payment method, refunds, voids, and cash variance." },
            RouteInfo { method: "GET", path: "/api/v1/pos/sales", owner: "pos", description: "List sales (paginated, filter by register_id, shift_id, cashier_id, status)." },
            RouteInfo { method: "POST", path: "/api/v1/pos/sales", owner: "pos", description: "Create a draft sale (cart) on a register, optionally in an open shift." },
            RouteInfo { method: "GET", path: "/api/v1/pos/sales/{id}", owner: "pos", description: "Get one sale with its lines and payments." },
            RouteInfo { method: "PATCH", path: "/api/v1/pos/sales/{id}", owner: "pos", description: "Update a draft sale (sale-level discount, customer, notes)." },
            RouteInfo { method: "POST", path: "/api/v1/pos/sales/{id}/items", owner: "pos", description: "Add a line to a draft sale (product, price, quantity, discount, tax rate)." },
            RouteInfo { method: "PATCH", path: "/api/v1/pos/sales/{id}/items/{item_id}", owner: "pos", description: "Update a draft sale line (quantity, price, discount, tax rate)." },
            RouteInfo { method: "DELETE", path: "/api/v1/pos/sales/{id}/items/{item_id}", owner: "pos", description: "Remove a line from a draft sale." },
            RouteInfo { method: "POST", path: "/api/v1/pos/sales/{id}/complete", owner: "pos", description: "Complete a sale with one or more payments (split payment, change); emits order.created, pos.sale.completed, stock decrement events, and creates the receipt." },
            RouteInfo { method: "POST", path: "/api/v1/pos/sales/{id}/void", owner: "pos", description: "Void a draft or held sale (completed sales use refund)." },
            RouteInfo { method: "POST", path: "/api/v1/pos/sales/{id}/refund", owner: "pos", description: "Refund a completed sale by line items or by amount; emits payment.refunded and stock increment events." },
            RouteInfo { method: "POST", path: "/api/v1/pos/sales/{id}/hold", owner: "pos", description: "Park a draft sale for later." },
            RouteInfo { method: "POST", path: "/api/v1/pos/sales/{id}/resume", owner: "pos", description: "Resume a held sale back to draft." },
            RouteInfo { method: "GET", path: "/api/v1/pos/receipts", owner: "pos", description: "List receipts (paginated, filter by register_id, cashier_id)." },
            RouteInfo { method: "GET", path: "/api/v1/pos/receipts/{sale_id}", owner: "pos", description: "Get the receipt data of a completed sale." },
            RouteInfo { method: "GET", path: "/api/v1/pos/receipts/{sale_id}/print", owner: "pos", description: "Get the structured printable receipt lines for a completed sale." },
            RouteInfo { method: "GET", path: "/api/v1/pos/products/search", owner: "pos", description: "Search the catalogue by text, barcode, or SKU through pos.product.search_requested; reports honestly when no catalogue handler answers." },
            RouteInfo { method: "GET", path: "/api/v1/pos/payment-methods", owner: "pos", description: "List the configured payment methods (paginated, filter by active)." },
            RouteInfo { method: "POST", path: "/api/v1/pos/payment-methods", owner: "pos", description: "Add a payment method definition (admin)." },
            RouteInfo { method: "PATCH", path: "/api/v1/pos/payment-methods/{id}", owner: "pos", description: "Update a payment method definition (admin)." },
            RouteInfo { method: "DELETE", path: "/api/v1/pos/payment-methods/{id}", owner: "pos", description: "Delete a payment method definition that no payment references (admin)." },
            RouteInfo { method: "GET", path: "/api/v1/pos/settings", owner: "pos", description: "Get the POS settings, including the offline-queueing capability flag." },
            RouteInfo { method: "PUT", path: "/api/v1/pos/settings", owner: "pos", description: "Update the POS settings (admin)." },
            RouteInfo { method: "GET", path: "/api/v1/pos/reports/end-of-day", owner: "pos", description: "End-of-day report: per-register totals by payment method, refunds, voids, and shift cash variances for one date." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(PosModule::new()));
}

// ---------------------------------------------------------------------------
// Register handlers
// ---------------------------------------------------------------------------

async fn list_registers(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<PosRegister> = state
            .registers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|register| {
                if let Some(status) = &status_filter {
                    if register.status != *status {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {} {}", register.name, register.code,
                        register.location.clone().unwrap_or_default()).to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["name", "code", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                "code" => a.code.to_lowercase().cmp(&b.code.to_lowercase()),
                _ => a.created_at.cmp(&b.created_at),
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

async fn create_register(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    body: web::Json<CreateRegisterRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosRegister> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let code = match &input.code {
            Some(raw) => require_text(raw, "code", MAX_CODE_LEN)?.to_uppercase(),
            None => format!("REG-{}", &new_id()[..8].to_uppercase()),
        };
        if state
            .registers
            .iter()
            .any(|entry| entry.value().code == code)
        {
            return Err(UnifiedError::conflict(format!(
                "Register code already exists: {code}"
            )));
        }
        let location = optional_text(&input.location, "location", MAX_LOCATION_LEN)?;
        let register = PosRegister {
            id: new_id(),
            name,
            code,
            location,
            status: "active".to_string(),
            cashiers: Vec::new(),
            created_at: now(),
            updated_at: now(),
        };
        state.registers.insert(register.id.clone(), register.clone());
        state.emit(
            "pos.register.created",
            json!({
                "register_id": register.id,
                "code": register.code,
                "name": register.name,
                "created_by": caller.actor,
            }),
        );
        Ok(register)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_register(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosRegister> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        load_register(&state, &path.into_inner())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_register(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRegisterRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosRegister> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_MANAGE)?;
        let id = path.into_inner();
        let input = body.into_inner();
        let mut register = load_register(&state, &id)?;
        if let Some(name) = &input.name {
            register.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(location) = &input.location {
            register.location = match location {
                Some(text) => Some(require_text(text, "location", MAX_LOCATION_LEN)?),
                None => None,
            };
        }
        if let Some(status) = &input.status {
            let status = status.trim().to_lowercase();
            if !matches!(status.as_str(), "active" | "inactive") {
                return Err(UnifiedError::bad_request(
                    "status must be one of: active, inactive",
                ));
            }
            if status == "inactive" && open_shift_for_register(&state, &id).is_some() {
                return Err(UnifiedError::unprocessable(
                    "Cannot deactivate a register with an open shift",
                ));
            }
            register.status = status;
        }
        register.updated_at = now();
        state.registers.insert(register.id.clone(), register.clone());
        state.emit(
            "pos.register.updated",
            json!({
                "register_id": register.id,
                "status": register.status,
                "updated_by": caller.actor,
            }),
        );
        Ok(register)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_register(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_MANAGE)?;
        let id = path.into_inner();
        let register = load_register(&state, &id)?;
        if open_shift_for_register(&state, &id).is_some() {
            return Err(UnifiedError::unprocessable(
                "Cannot delete a register with an open shift",
            ));
        }
        if state
            .sales
            .iter()
            .any(|entry| entry.value().register_id == id)
        {
            return Err(UnifiedError::unprocessable(
                "Cannot delete a register that has sales; deactivate it instead",
            ));
        }
        state.registers.remove(&id);
        state.emit(
            "pos.register.deleted",
            json!({
                "register_id": id,
                "code": register.code,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "register_id": id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn assign_cashier(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AssignCashierRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosRegister> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_MANAGE)?;
        let id = path.into_inner();
        let user_id = require_text(&body.into_inner().user_id, "user_id", MAX_CODE_LEN)?;
        let mut register = load_register(&state, &id)?;
        if !register.cashiers.iter().any(|cashier| cashier == &user_id) {
            register.cashiers.push(user_id.clone());
        }
        register.updated_at = now();
        state.registers.insert(register.id.clone(), register.clone());
        state.emit(
            "pos.register.cashier_assigned",
            json!({
                "register_id": register.id,
                "user_id": user_id,
                "assigned_by": caller.actor,
            }),
        );
        Ok(register)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn unassign_cashier(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AssignCashierRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosRegister> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_MANAGE)?;
        let id = path.into_inner();
        let user_id = require_text(&body.into_inner().user_id, "user_id", MAX_CODE_LEN)?;
        let mut register = load_register(&state, &id)?;
        // A cashier with an open shift on this register cannot be
        // removed until the shift is closed.
        if let Some(shift) = open_shift_for_register(&state, &id) {
            if shift.cashier_id == user_id {
                return Err(UnifiedError::unprocessable(
                    "Cannot remove a cashier with an open shift on this register",
                ));
            }
        }
        register.cashiers.retain(|cashier| cashier != &user_id);
        register.updated_at = now();
        state.registers.insert(register.id.clone(), register.clone());
        state.emit(
            "pos.register.cashier_unassigned",
            json!({
                "register_id": register.id,
                "user_id": user_id,
                "unassigned_by": caller.actor,
            }),
        );
        Ok(register)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Shift handlers
// ---------------------------------------------------------------------------

async fn list_shifts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        let params = query.into_inner();
        let register_filter = params.filters.get("register_id").cloned();
        let cashier_filter = params.filters.get("cashier_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<PosShift> = state
            .shifts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|shift| {
                if let Some(register_id) = &register_filter {
                    if shift.register_id != *register_id {
                        return false;
                    }
                }
                if let Some(cashier_id) = &cashier_filter {
                    if shift.cashier_id != *cashier_id {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if shift.status != *status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.opened_at.cmp(&a.opened_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn open_shift(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    body: web::Json<OpenShiftRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosShift> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let input = body.into_inner();
        let register = load_register(&state, &input.register_id)?;
        if register.status != "active" {
            return Err(UnifiedError::unprocessable(
                "Cannot open a shift on an inactive register",
            ));
        }
        caller_may_sell(&ctx, &state, &register, &caller)?;
        if open_shift_for_register(&state, &register.id).is_some() {
            return Err(UnifiedError::conflict(
                "Register already has an open shift",
            ));
        }
        let opening_float = valid_money(input.opening_float.unwrap_or(0.0), "opening_float")?;
        let currency = match &input.currency {
            Some(raw) => valid_currency(raw)?,
            None => state.settings().default_currency,
        };
        let notes = optional_text(&input.notes, "notes", MAX_NOTES_LEN)?;
        let shift = PosShift {
            id: new_id(),
            register_id: register.id.clone(),
            cashier_id: caller.actor.clone(),
            status: "open".to_string(),
            opening_float,
            currency,
            expected_cash: None,
            counted_cash: None,
            variance: None,
            notes,
            opened_at: now(),
            closed_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state.shifts.insert(shift.id.clone(), shift.clone());
        state.emit(
            "pos.shift.opened",
            json!({
                "shift_id": shift.id,
                "register_id": shift.register_id,
                "cashier_id": shift.cashier_id,
                "opening_float": shift.opening_float,
                "currency": shift.currency,
            }),
        );
        Ok(shift)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_shift(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosShift> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        load_shift(&state, &path.into_inner())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn close_shift(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CloseShiftRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let id = path.into_inner();
        let input = body.into_inner();
        let mut shift = load_shift(&state, &id)?;
        if shift.status != "open" {
            return Err(UnifiedError::unprocessable(
                "Shift is already closed",
            ));
        }
        if shift.cashier_id != caller.actor
            && !check_capability(&ctx, &caller.role, PERM_POS_MANAGE)
        {
            return Err(UnifiedError::forbidden(
                "Only the shift cashier or a manager may close this shift",
            ));
        }
        // A shift with open (draft or held) sales cannot close:
        // every cart must be completed or voided first.
        let open_sales = state
            .sales
            .iter()
            .filter(|entry| {
                entry.value().shift_id.as_deref() == Some(shift.id.as_str())
                    && matches!(entry.value().status.as_str(), "draft" | "held")
            })
            .count();
        if open_sales > 0 {
            return Err(UnifiedError::unprocessable(format!(
                "Shift has {open_sales} open sale(s); complete or void them before closing"
            )));
        }
        let counted_cash = valid_money(input.counted_cash, "counted_cash")?;
        let expected_cash = expected_cash_for_shift(&state, &shift);
        let variance = round_money(counted_cash - expected_cash);
        shift.expected_cash = Some(expected_cash);
        shift.counted_cash = Some(counted_cash);
        shift.variance = Some(variance);
        shift.status = "closed".to_string();
        shift.closed_at = Some(now());
        if let Some(notes) = optional_text(&input.notes, "notes", MAX_NOTES_LEN)? {
            shift.notes = Some(notes);
        }
        shift.updated_at = now();
        state.shifts.insert(shift.id.clone(), shift.clone());
        let summary = shift_summary(&state, &shift);
        state.emit(
            "pos.shift.closed",
            json!({
                "shift_id": shift.id,
                "register_id": shift.register_id,
                "cashier_id": shift.cashier_id,
                "expected_cash": expected_cash,
                "counted_cash": counted_cash,
                "variance": variance,
                "currency": shift.currency,
                "closed_by": caller.actor,
            }),
        );
        Ok(json!({ "shift": shift, "summary": summary }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_shift_summary(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        let shift = load_shift(&state, &path.into_inner())?;
        Ok(shift_summary(&state, &shift))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Sale handlers
// ---------------------------------------------------------------------------

async fn list_sales(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        let params = query.into_inner();
        let register_filter = params.filters.get("register_id").cloned();
        let shift_filter = params.filters.get("shift_id").cloned();
        let cashier_filter = params.filters.get("cashier_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<PosSale> = state
            .sales
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|sale| {
                if let Some(register_id) = &register_filter {
                    if sale.register_id != *register_id {
                        return false;
                    }
                }
                if let Some(shift_id) = &shift_filter {
                    if sale.shift_id.as_deref() != Some(shift_id.as_str()) {
                        return false;
                    }
                }
                if let Some(cashier_id) = &cashier_filter {
                    if sale.cashier_id != *cashier_id {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if sale.status != *status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_sale(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    body: web::Json<CreateSaleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosSale> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let input = body.into_inner();
        let settings = state.settings();
        let register = load_register(&state, &input.register_id)?;
        if register.status != "active" {
            return Err(UnifiedError::unprocessable(
                "Cannot create a sale on an inactive register",
            ));
        }
        caller_may_sell(&ctx, &state, &register, &caller)?;
        let shift_id = match &input.shift_id {
            Some(raw) => {
                let shift = load_shift(&state, raw)?;
                if shift.register_id != register.id {
                    return Err(UnifiedError::bad_request(
                        "Shift does not belong to this register",
                    ));
                }
                if shift.status != "open" {
                    return Err(UnifiedError::unprocessable(
                        "Cannot create a sale in a closed shift",
                    ));
                }
                if shift.cashier_id != caller.actor
                    && !check_capability(&ctx, &caller.role, PERM_POS_MANAGE)
                {
                    return Err(UnifiedError::forbidden(
                        "Shift belongs to a different cashier",
                    ));
                }
                Some(shift.id)
            }
            None => open_shift_for_register(&state, &register.id).map(|shift| shift.id),
        };
        let currency = match &input.currency {
            Some(raw) => valid_currency(raw)?,
            None => settings.default_currency.clone(),
        };
        let discount_type = valid_discount_type(&input.discount_type)?;
        let discount_value = input.discount_value.unwrap_or(0.0);
        if discount_type.is_some() && (!discount_value.is_finite() || discount_value < 0.0) {
            return Err(UnifiedError::bad_request(
                "discount_value must be zero or greater",
            ));
        }
        let customer_name = optional_text(&input.customer_name, "customer_name", MAX_NAME_LEN)?;
        let notes = optional_text(&input.notes, "notes", MAX_NOTES_LEN)?;
        let company_id = optional_text(&input.company_id, "company_id", MAX_CODE_LEN)?;
        let mut items: Vec<PosSaleItem> = Vec::new();
        if let Some(raw_items) = &input.items {
            for raw in raw_items {
                items.push(build_sale_item(raw, &settings)?);
            }
        }
        let mut sale = PosSale {
            id: new_id(),
            number: state.next_sale_number(),
            register_id: register.id.clone(),
            shift_id,
            cashier_id: caller.actor.clone(),
            company_id,
            items,
            subtotal: 0.0,
            discount_type,
            discount_value,
            discount_total: 0.0,
            tax_total: 0.0,
            total: 0.0,
            currency,
            tax_inclusive: settings.tax_inclusive,
            status: "draft".to_string(),
            amount_paid: 0.0,
            change_due: 0.0,
            refunded_amount: 0.0,
            customer_name,
            notes,
            completed_at: None,
            voided_at: None,
            created_at: now(),
            updated_at: now(),
        };
        recompute_sale(&mut sale, &settings);
        state.sales.insert(sale.id.clone(), sale.clone());
        state.emit(
            "pos.sale.created",
            json!({
                "sale_id": sale.id,
                "number": sale.number,
                "register_id": sale.register_id,
                "shift_id": sale.shift_id,
                "cashier_id": sale.cashier_id,
                "total": sale.total,
                "currency": sale.currency,
            }),
        );
        Ok(sale)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_sale(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        let sale = load_sale(&state, &path.into_inner())?;
        let payments: Vec<PosPayment> = state
            .payments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|payment| payment.sale_id == sale.id)
            .collect();
        let refunds: Vec<PosRefund> = state
            .refunds
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|refund| refund.sale_id == sale.id)
            .collect();
        Ok(json!({ "sale": sale, "payments": payments, "refunds": refunds }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// A draft sale may be edited by its cashier or a manager.
fn load_editable_sale(
    ctx: &AppContext,
    state: &PosState,
    id: &str,
    caller: &Caller,
) -> UnifiedResult<PosSale> {
    let sale = load_sale(state, id)?;
    if !matches!(sale.status.as_str(), "draft" | "held") {
        return Err(UnifiedError::unprocessable(format!(
            "Sale {} cannot be edited (status: {})",
            sale.number, sale.status
        )));
    }
    caller_may_act_on_sale(ctx, &sale, caller)?;
    Ok(sale)
}

async fn update_sale(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateSaleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosSale> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let id = path.into_inner();
        let input = body.into_inner();
        let settings = state.settings();
        let mut sale = load_editable_sale(&ctx, &state, &id, &caller)?;
        if let Some(discount_type) = &input.discount_type {
            sale.discount_type = valid_discount_type(discount_type)?;
        }
        if let Some(discount_value) = input.discount_value {
            if !discount_value.is_finite() || discount_value < 0.0 {
                return Err(UnifiedError::bad_request(
                    "discount_value must be zero or greater",
                ));
            }
            sale.discount_value = discount_value;
        }
        if let Some(customer_name) = &input.customer_name {
            sale.customer_name = match customer_name {
                Some(text) => Some(require_text(text, "customer_name", MAX_NAME_LEN)?),
                None => None,
            };
        }
        if let Some(notes) = &input.notes {
            sale.notes = match notes {
                Some(text) => Some(require_text(text, "notes", MAX_NOTES_LEN)?),
                None => None,
            };
        }
        recompute_sale(&mut sale, &settings);
        sale.updated_at = now();
        state.sales.insert(sale.id.clone(), sale.clone());
        state.emit(
            "pos.sale.updated",
            json!({
                "sale_id": sale.id,
                "number": sale.number,
                "total": sale.total,
                "currency": sale.currency,
                "updated_by": caller.actor,
            }),
        );
        Ok(sale)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn add_sale_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<NewSaleItemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosSale> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let id = path.into_inner();
        let settings = state.settings();
        let mut sale = load_editable_sale(&ctx, &state, &id, &caller)?;
        let item = build_sale_item(&body.into_inner(), &settings)?;
        sale.items.push(item);
        recompute_sale(&mut sale, &settings);
        sale.updated_at = now();
        state.sales.insert(sale.id.clone(), sale.clone());
        Ok(sale)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_sale_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateSaleItemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosSale> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let (id, item_id) = path.into_inner();
        let input = body.into_inner();
        let settings = state.settings();
        let mut sale = load_editable_sale(&ctx, &state, &id, &caller)?;
        let position = sale
            .items
            .iter()
            .position(|item| item.id == item_id)
            .ok_or_else(|| UnifiedError::not_found(format!("Sale line not found: {item_id}")))?;
        let item = &mut sale.items[position];
        if let Some(quantity) = input.quantity {
            if quantity == 0 || quantity > 10_000 {
                return Err(UnifiedError::bad_request(
                    "quantity must be between 1 and 10000",
                ));
            }
            item.quantity = quantity;
        }
        if let Some(unit_price) = input.unit_price {
            item.unit_price = valid_money(unit_price, "unit_price")?;
        }
        if let Some(discount_type) = &input.discount_type {
            item.discount_type = valid_discount_type(discount_type)?;
        }
        if let Some(discount_value) = input.discount_value {
            if !discount_value.is_finite() || discount_value < 0.0 {
                return Err(UnifiedError::bad_request(
                    "discount_value must be zero or greater",
                ));
            }
            item.discount_value = discount_value;
        }
        if let Some(tax_rate) = input.tax_rate {
            if !tax_rate.is_finite() || tax_rate < 0.0 || tax_rate > 10.0 {
                return Err(UnifiedError::bad_request(
                    "tax_rate must be a finite fraction between 0 and 10",
                ));
            }
            item.tax_rate = tax_rate;
        }
        let gross = round_money(item.unit_price * item.quantity as f64);
        item.discount_amount = discount_amount(
            &item.discount_type,
            item.discount_value,
            gross,
            settings.max_discount_percent,
        )?;
        recompute_sale(&mut sale, &settings);
        sale.updated_at = now();
        state.sales.insert(sale.id.clone(), sale.clone());
        Ok(sale)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn remove_sale_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosSale> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let (id, item_id) = path.into_inner();
        let settings = state.settings();
        let mut sale = load_editable_sale(&ctx, &state, &id, &caller)?;
        let before = sale.items.len();
        sale.items.retain(|item| item.id != item_id);
        if sale.items.len() == before {
            return Err(UnifiedError::not_found(format!(
                "Sale line not found: {item_id}"
            )));
        }
        recompute_sale(&mut sale, &settings);
        sale.updated_at = now();
        state.sales.insert(sale.id.clone(), sale.clone());
        Ok(sale)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn complete_sale(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CompleteSaleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let id = path.into_inner();
        let input = body.into_inner();
        let settings = state.settings();
        state.seed_payment_methods();
        let mut sale = load_sale(&state, &id)?;
        if !matches!(sale.status.as_str(), "draft" | "held") {
            return Err(UnifiedError::unprocessable(format!(
                "Sale {} cannot be completed (status: {})",
                sale.number, sale.status
            )));
        }
        caller_may_act_on_sale(&ctx, &sale, &caller)?;
        if sale.items.is_empty() {
            return Err(UnifiedError::unprocessable(
                "Cannot complete a sale with no lines",
            ));
        }
        if let Some(shift_id) = &sale.shift_id {
            let shift = load_shift(&state, shift_id)?;
            if shift.status != "open" {
                return Err(UnifiedError::unprocessable(
                    "Cannot complete a sale in a closed shift",
                ));
            }
        }
        if input.payments.is_empty() {
            return Err(UnifiedError::bad_request(
                "At least one payment is required",
            ));
        }
        if !settings.allow_split_payment && input.payments.len() > 1 {
            return Err(UnifiedError::unprocessable(
                "Split payment is disabled in the POS settings",
            ));
        }
        recompute_sale(&mut sale, &settings);
        // Validate every payment against the stored method
        // definitions, then require the applied amounts to sum to
        // the sale total. Change is the tendered excess, so a card
        // overpayment is rejected while cash change is exact.
        let mut payments: Vec<PosPayment> = Vec::new();
        let mut applied_total = 0.0;
        let mut tendered_total = 0.0;
        for raw in &input.payments {
            let method = state
                .payment_methods
                .get(&raw.method_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| {
                    UnifiedError::not_found(format!(
                        "Payment method not found: {}",
                        raw.method_id
                    ))
                })?;
            if !method.active {
                return Err(UnifiedError::unprocessable(format!(
                    "Payment method is disabled: {}",
                    method.label
                )));
            }
            let amount = valid_money(raw.amount, "payment amount")?;
            if amount <= 0.0 {
                return Err(UnifiedError::bad_request(
                    "payment amount must be greater than zero",
                ));
            }
            let tendered = match raw.tendered {
                Some(value) => valid_money(value, "tendered amount")?,
                None => amount,
            };
            if tendered < amount {
                return Err(UnifiedError::bad_request(
                    "tendered amount must not be less than the payment amount",
                ));
            }
            if method.requires_reference {
                let reference = raw
                    .reference
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                if reference.is_none() {
                    return Err(UnifiedError::bad_request(format!(
                        "Payment method \"{}\" requires a reference",
                        method.label
                    )));
                }
            }
            applied_total += amount;
            tendered_total += tendered;
            payments.push(PosPayment {
                id: new_id(),
                sale_id: sale.id.clone(),
                method_id: method.id.clone(),
                method_code: method.code.clone(),
                amount,
                tendered,
                reference: optional_text(&raw.reference, "reference", MAX_CODE_LEN)?,
                recorded_by: caller.actor.clone(),
                recorded_at: now(),
            });
        }
        applied_total = round_money(applied_total);
        if (applied_total - sale.total).abs() > MONEY_EPSILON {
            return Err(UnifiedError::unprocessable(format!(
                "Payments total {} {} but the sale total is {} {}",
                applied_total, sale.currency, sale.total, sale.currency
            )));
        }
        let change_due = round_money(tendered_total - applied_total);
        let completed_at = now();
        sale.status = "completed".to_string();
        sale.amount_paid = applied_total;
        sale.change_due = change_due;
        sale.completed_at = Some(completed_at);
        if let Some(notes) = optional_text(&input.notes, "notes", MAX_NOTES_LEN)? {
            sale.notes = Some(notes);
        }
        sale.updated_at = completed_at;
        for payment in &payments {
            state
                .payments
                .insert(payment.id.clone(), payment.clone());
        }
        state.sales.insert(sale.id.clone(), sale.clone());
        // Receipt: tax grouped by rate, plus the structured print
        // lines for the printer.
        let mut tax_breakdown: Vec<TaxBreakdownLine> = Vec::new();
        for item in &sale.items {
            if let Some(line) = tax_breakdown
                .iter_mut()
                .find(|line| (line.tax_rate - item.tax_rate).abs() < f64::EPSILON)
            {
                line.tax_amount = round_money(line.tax_amount + item.tax_amount);
            } else {
                tax_breakdown.push(TaxBreakdownLine {
                    tax_rate: item.tax_rate,
                    tax_amount: item.tax_amount,
                });
            }
        }
        tax_breakdown.sort_by(|a, b| {
            a.tax_rate
                .partial_cmp(&b.tax_rate)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let register = load_register(&state, &sale.register_id)?;
        let mut receipt = PosReceipt {
            id: new_id(),
            number: state.next_receipt_number(),
            sale_id: sale.id.clone(),
            sale_number: sale.number.clone(),
            register_id: sale.register_id.clone(),
            register_code: Some(register.code.clone()),
            cashier_id: sale.cashier_id.clone(),
            store_name: settings.store_name.clone(),
            store_address: settings.store_address.clone(),
            items: sale.items.clone(),
            subtotal: sale.subtotal,
            discount_total: sale.discount_total,
            tax_total: sale.tax_total,
            total: sale.total,
            currency: sale.currency.clone(),
            tax_breakdown,
            payments: payments.clone(),
            amount_paid: applied_total,
            change_due,
            print_lines: Vec::new(),
            footer: settings.receipt_footer.clone(),
            issued_at: completed_at,
            created_at: completed_at,
        };
        receipt.print_lines = build_print_lines(&receipt, &settings);
        state.receipts.insert(receipt.id.clone(), receipt.clone());
        // Events. The store-shaped `order.created` payload is what
        // the store, billing, and tax modules consume; the POS
        // events carry the full sale for POS listeners.
        let order_items: Vec<Value> = sale
            .items
            .iter()
            .map(|item| {
                json!({
                    "product_id": item.product_id,
                    "name": item.name,
                    "unit_price": item.unit_price,
                    "quantity": item.quantity,
                    "line_total": item.line_total,
                    "license_product": false,
                })
            })
            .collect();
        state.emit(
            "order.created",
            json!({
                "id": sale.id,
                "user_id": sale.cashier_id,
                "company_id": sale.company_id,
                "items": order_items,
                "subtotal": sale.subtotal,
                "discount_total": sale.discount_total,
                "total": sale.total,
                "currency": sale.currency,
                "status": "paid",
                "provider": "pos",
                "pos_sale_number": sale.number,
                "pos_register_id": sale.register_id,
                "pos_shift_id": sale.shift_id,
            }),
        );
        state.emit(
            EVENT_SALE_COMPLETED,
            json!({
                "sale_id": sale.id,
                "number": sale.number,
                "register_id": sale.register_id,
                "shift_id": sale.shift_id,
                "cashier_id": sale.cashier_id,
                "total": sale.total,
                "currency": sale.currency,
                "amount_paid": applied_total,
                "change_due": change_due,
                "receipt_id": receipt.id,
                "receipt_number": receipt.number,
                "items": serde_json::to_value(&sale.items).unwrap_or(Value::Null),
                "payments": serde_json::to_value(&payments).unwrap_or(Value::Null),
            }),
        );
        state.emit(
            "pos.receipt.created",
            json!({
                "receipt_id": receipt.id,
                "receipt_number": receipt.number,
                "sale_id": sale.id,
                "register_id": sale.register_id,
                "total": sale.total,
                "currency": sale.currency,
            }),
        );
        // Stock moves are requested, never applied here.
        for item in &sale.items {
            state.emit(
                EVENT_STOCK_DECREMENT_REQUESTED,
                json!({
                    "asset_id": item.product_id,
                    "product_id": item.product_id,
                    "sku": item.sku,
                    "barcode": item.barcode,
                    "quantity": item.quantity,
                    "reason": format!("POS sale {}", sale.number),
                    "sale_id": sale.id,
                    "register_id": sale.register_id,
                }),
            );
        }
        Ok(json!({
            "sale": sale,
            "payments": payments,
            "receipt": receipt,
            "change_due": change_due,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn void_sale(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosSale> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let id = path.into_inner();
        let mut sale = load_sale(&state, &id)?;
        if !matches!(sale.status.as_str(), "draft" | "held") {
            return Err(UnifiedError::unprocessable(format!(
                "Sale {} cannot be voided (status: {}); use refund for completed sales",
                sale.number, sale.status
            )));
        }
        caller_may_act_on_sale(&ctx, &sale, &caller)?;
        sale.status = "voided".to_string();
        sale.voided_at = Some(now());
        sale.updated_at = now();
        state.sales.insert(sale.id.clone(), sale.clone());
        state.emit(
            "pos.sale.voided",
            json!({
                "sale_id": sale.id,
                "number": sale.number,
                "register_id": sale.register_id,
                "shift_id": sale.shift_id,
                "cashier_id": sale.cashier_id,
                "voided_by": caller.actor,
            }),
        );
        Ok(sale)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn refund_sale(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RefundSaleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_REFUND)?;
        let id = path.into_inner();
        let input = body.into_inner();
        state.seed_payment_methods();
        let mut sale = load_sale(&state, &id)?;
        if !matches!(
            sale.status.as_str(),
            "completed" | "partially_refunded"
        ) {
            return Err(UnifiedError::unprocessable(format!(
                "Sale {} cannot be refunded (status: {})",
                sale.number, sale.status
            )));
        }
        let reason = require_text(
            input.reason.as_deref().unwrap_or("Customer return"),
            "reason",
            MAX_REASON_LEN,
        )?;
        let remaining = round_money(sale.total - sale.refunded_amount);
        if remaining <= 0.0 {
            return Err(UnifiedError::unprocessable(
                "Sale has already been fully refunded",
            ));
        }
        // Refund amount: by line items when given, otherwise the
        // plain amount. A line refund returns the pro-rata share of
        // the line total (including its tax) per unit.
        let mut refund_items: Vec<PosRefundItem> = Vec::new();
        let mut amount = 0.0;
        if let Some(raw_items) = &input.items {
            if raw_items.is_empty() {
                return Err(UnifiedError::bad_request(
                    "items must not be empty when given",
                ));
            }
            for raw in raw_items {
                if raw.quantity == 0 {
                    return Err(UnifiedError::bad_request(
                        "refund quantity must be greater than zero",
                    ));
                }
                let item = sale
                    .items
                    .iter()
                    .find(|item| item.id == raw.item_id)
                    .ok_or_else(|| {
                        UnifiedError::not_found(format!("Sale line not found: {}", raw.item_id))
                    })?;
                let available = item.quantity - item.refunded_quantity;
                if raw.quantity > available {
                    return Err(UnifiedError::unprocessable(format!(
                        "Only {available} unit(s) of \"{}\" are left to refund",
                        item.name
                    )));
                }
                let unit_total = round_money(item.line_total / item.quantity as f64);
                let line_amount = round_money(unit_total * raw.quantity as f64);
                amount += line_amount;
                refund_items.push(PosRefundItem {
                    item_id: item.id.clone(),
                    product_id: item.product_id.clone(),
                    quantity: raw.quantity,
                    amount: line_amount,
                });
            }
        } else if let Some(raw_amount) = input.amount {
            amount = valid_money(raw_amount, "amount")?;
            if amount <= 0.0 {
                return Err(UnifiedError::bad_request(
                    "amount must be greater than zero",
                ));
            }
        } else {
            return Err(UnifiedError::bad_request(
                "Provide either items or amount",
            ));
        }
        amount = round_money(amount);
        if amount > remaining + MONEY_EPSILON {
            return Err(UnifiedError::unprocessable(format!(
                "Refund amount {} {} exceeds the refundable balance {} {}",
                amount, sale.currency, remaining, sale.currency
            )));
        }
        // Refund method: the requested method, else the method of
        // the sale's first payment.
        let method = match &input.method_id {
            Some(raw) => state
                .payment_methods
                .get(raw)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| {
                    UnifiedError::not_found(format!("Payment method not found: {raw}"))
                })?,
            None => {
                let first = state
                    .payments
                    .iter()
                    .map(|entry| entry.value().clone())
                    .find(|payment| payment.sale_id == sale.id)
                    .ok_or_else(|| {
                        UnifiedError::unprocessable("Sale has no payment to refund against")
                    })?;
                state
                    .payment_methods
                    .get(&first.method_id)
                    .map(|entry| entry.value().clone())
                    .ok_or_else(|| {
                        UnifiedError::not_found("Original payment method no longer exists")
                    })?
            }
        };
        if !method.active {
            return Err(UnifiedError::unprocessable(format!(
                "Payment method is disabled: {}",
                method.label
            )));
        }
        for refund_item in &refund_items {
            if let Some(item) = sale
                .items
                .iter_mut()
                .find(|item| item.id == refund_item.item_id)
            {
                item.refunded_quantity += refund_item.quantity;
            }
        }
        sale.refunded_amount = round_money(sale.refunded_amount + amount);
        sale.status = if (sale.total - sale.refunded_amount).abs() <= MONEY_EPSILON {
            "refunded".to_string()
        } else {
            "partially_refunded".to_string()
        };
        sale.updated_at = now();
        let refund = PosRefund {
            id: new_id(),
            sale_id: sale.id.clone(),
            shift_id: sale.shift_id.clone(),
            amount,
            method_id: method.id.clone(),
            method_code: method.code.clone(),
            items: refund_items.clone(),
            reason: reason.clone(),
            refunded_by: caller.actor.clone(),
            created_at: now(),
        };
        state.refunds.insert(refund.id.clone(), refund.clone());
        state.sales.insert(sale.id.clone(), sale.clone());
        state.emit(
            "pos.sale.refunded",
            json!({
                "sale_id": sale.id,
                "number": sale.number,
                "refund_id": refund.id,
                "amount": amount,
                "currency": sale.currency,
                "method": method.code,
                "status": sale.status,
                "reason": reason,
                "refunded_by": caller.actor,
            }),
        );
        state.emit(
            "payment.refunded",
            json!({
                "order_id": sale.id,
                "sale_id": sale.id,
                "amount": amount,
                "currency": sale.currency,
                "method": method.code,
                "reason": reason,
            }),
        );
        // Returned goods go back to stock through the inventory
        // side, never by writing stock here.
        for refund_item in &refund_items {
            let item = sale
                .items
                .iter()
                .find(|item| item.id == refund_item.item_id);
            state.emit(
                EVENT_STOCK_INCREMENT_REQUESTED,
                json!({
                    "asset_id": refund_item.product_id,
                    "product_id": refund_item.product_id,
                    "sku": item.and_then(|item| item.sku.clone()),
                    "barcode": item.and_then(|item| item.barcode.clone()),
                    "quantity": refund_item.quantity,
                    "reason": format!("POS refund of sale {}", sale.number),
                    "sale_id": sale.id,
                    "refund_id": refund.id,
                }),
            );
        }
        Ok(json!({ "sale": sale, "refund": refund }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn hold_sale(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosSale> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let id = path.into_inner();
        let mut sale = load_sale(&state, &id)?;
        if sale.status != "draft" {
            return Err(UnifiedError::unprocessable(format!(
                "Only a draft sale can be held (status: {})",
                sale.status
            )));
        }
        caller_may_act_on_sale(&ctx, &sale, &caller)?;
        sale.status = "held".to_string();
        sale.updated_at = now();
        state.sales.insert(sale.id.clone(), sale.clone());
        state.emit(
            "pos.sale.held",
            json!({
                "sale_id": sale.id,
                "number": sale.number,
                "held_by": caller.actor,
            }),
        );
        Ok(sale)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn resume_sale(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosSale> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let id = path.into_inner();
        let mut sale = load_sale(&state, &id)?;
        if sale.status != "held" {
            return Err(UnifiedError::unprocessable(format!(
                "Only a held sale can be resumed (status: {})",
                sale.status
            )));
        }
        caller_may_act_on_sale(&ctx, &sale, &caller)?;
        sale.status = "draft".to_string();
        sale.updated_at = now();
        state.sales.insert(sale.id.clone(), sale.clone());
        state.emit(
            "pos.sale.resumed",
            json!({
                "sale_id": sale.id,
                "number": sale.number,
                "resumed_by": caller.actor,
            }),
        );
        Ok(sale)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Receipt handlers
// ---------------------------------------------------------------------------

fn load_receipt_by_sale(state: &PosState, sale_id: &str) -> UnifiedResult<PosReceipt> {
    state
        .receipts
        .iter()
        .map(|entry| entry.value().clone())
        .find(|receipt| receipt.sale_id == sale_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Receipt not found for sale: {sale_id}")))
}

async fn list_receipts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        let params = query.into_inner();
        let register_filter = params.filters.get("register_id").cloned();
        let cashier_filter = params.filters.get("cashier_id").cloned();
        let mut items: Vec<PosReceipt> = state
            .receipts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|receipt| {
                if let Some(register_id) = &register_filter {
                    if receipt.register_id != *register_id {
                        return false;
                    }
                }
                if let Some(cashier_id) = &cashier_filter {
                    if receipt.cashier_id != *cashier_id {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.issued_at.cmp(&a.issued_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_receipt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosReceipt> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        load_receipt_by_sale(&state, &path.into_inner())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_receipt_print(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        let receipt = load_receipt_by_sale(&state, &path.into_inner())?;
        Ok(json!({
            "receipt_number": receipt.number,
            "sale_number": receipt.sale_number,
            "sale_id": receipt.sale_id,
            "issued_at": receipt.issued_at,
            "currency": receipt.currency,
            "total": receipt.total,
            "width_chars": state.settings().receipt_width_chars,
            "print_lines": receipt.print_lines,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Product search (hook-driven catalogue lookup)
// ---------------------------------------------------------------------------

async fn search_products(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    query: web::Query<ProductSearchQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let input = query.into_inner();
        let limit = input.limit.unwrap_or(20).clamp(1, 100);
        let has_query = input
            .q
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some()
            || input
                .barcode
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_some()
            || input
                .sku
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_some();
        if !has_query {
            return Err(UnifiedError::bad_request(
                "Provide q, barcode, or sku to search the catalogue",
            ));
        }
        // Ask the catalogue side through the hook bus. The store /
        // products side answers `pos.product.search_requested` with
        // a result carrying a `products` array; when nothing
        // answers, the endpoint reports that honestly instead of
        // inventing catalogue rows.
        let mut products: Vec<Value> = Vec::new();
        let mut answered = false;
        if let Some(bus) = state.bus() {
            let answer = bus.emit(
                "pos.product.search_requested",
                json!({
                    "q": input.q,
                    "barcode": input.barcode,
                    "sku": input.sku,
                    "limit": limit,
                    "requested_by": caller.actor,
                }),
            );
            for result in &answer.results {
                if let Some(items) = result.get("products").and_then(Value::as_array) {
                    answered = true;
                    products.extend(items.iter().cloned());
                }
            }
        }
        products.truncate(limit as usize);
        Ok(json!({
            "products": products,
            "catalogue_available": answered,
            "note": if answered {
                Value::Null
            } else {
                Value::String(
                    "No catalogue handler answered pos.product.search_requested; \
                     product lookup is unavailable until the store/products side registers one."
                        .to_string(),
                )
            },
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Payment-method handlers (admin configuration)
// ---------------------------------------------------------------------------

async fn list_payment_methods(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        state.seed_payment_methods();
        let params = query.into_inner();
        let active_filter = params.filters.get("active").cloned();
        let mut items: Vec<PosPaymentMethod> = state
            .payment_methods
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|method| {
                if let Some(active) = &active_filter {
                    let wanted = active.trim().to_lowercase() == "true";
                    if method.active != wanted {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_payment_method(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    body: web::Json<CreatePaymentMethodRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosPaymentMethod> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_MANAGE)?;
        state.seed_payment_methods();
        let input = body.into_inner();
        let code = require_text(&input.code, "code", MAX_CODE_LEN)?.to_lowercase();
        if !code
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(UnifiedError::bad_request(
                "code must contain only lowercase letters, digits, and underscores",
            ));
        }
        if state
            .payment_methods
            .iter()
            .any(|entry| entry.value().code == code)
        {
            return Err(UnifiedError::conflict(format!(
                "Payment method code already exists: {code}"
            )));
        }
        let label = require_text(&input.label, "label", MAX_LABEL_LEN)?;
        let kind = match &input.kind {
            Some(raw) => require_text(raw, "kind", MAX_CODE_LEN)?.to_lowercase(),
            None => "other".to_string(),
        };
        let method = PosPaymentMethod {
            id: new_id(),
            code,
            label,
            kind,
            is_cash: input.is_cash.unwrap_or(false),
            requires_reference: input.requires_reference.unwrap_or(false),
            active: input.active.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state
            .payment_methods
            .insert(method.id.clone(), method.clone());
        state.emit(
            "pos.payment_method.created",
            json!({
                "method_id": method.id,
                "code": method.code,
                "created_by": caller.actor,
            }),
        );
        Ok(method)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn update_payment_method(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePaymentMethodRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosPaymentMethod> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_MANAGE)?;
        let id = path.into_inner();
        let input = body.into_inner();
        let mut method = state
            .payment_methods
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Payment method not found: {id}")))?;
        if let Some(label) = &input.label {
            method.label = require_text(label, "label", MAX_LABEL_LEN)?;
        }
        if let Some(kind) = &input.kind {
            method.kind = require_text(kind, "kind", MAX_CODE_LEN)?.to_lowercase();
        }
        if let Some(is_cash) = input.is_cash {
            method.is_cash = is_cash;
        }
        if let Some(requires_reference) = input.requires_reference {
            method.requires_reference = requires_reference;
        }
        if let Some(active) = input.active {
            if !active
                && state
                    .payments
                    .iter()
                    .any(|entry| entry.value().method_id == id)
            {
                return Err(UnifiedError::unprocessable(
                    "Cannot disable a payment method that completed sales used; delete is also blocked, keep it enabled or leave it unused",
                ));
            }
            method.active = active;
        }
        method.updated_at = now();
        state
            .payment_methods
            .insert(method.id.clone(), method.clone());
        state.emit(
            "pos.payment_method.updated",
            json!({
                "method_id": method.id,
                "code": method.code,
                "active": method.active,
                "updated_by": caller.actor,
            }),
        );
        Ok(method)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_payment_method(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_MANAGE)?;
        let id = path.into_inner();
        let method = state
            .payment_methods
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Payment method not found: {id}")))?;
        if state
            .payments
            .iter()
            .any(|entry| entry.value().method_id == id)
            || state
                .refunds
                .iter()
                .any(|entry| entry.value().method_id == id)
        {
            return Err(UnifiedError::unprocessable(
                "Cannot delete a payment method that sales or refunds reference; disable it instead",
            ));
        }
        state.payment_methods.remove(&id);
        state.emit(
            "pos.payment_method.deleted",
            json!({
                "method_id": id,
                "code": method.code,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "method_id": id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Settings handlers
// ---------------------------------------------------------------------------

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        let settings = state.settings();
        Ok(json!({
            "settings": settings,
            "capabilities": {
                // Honest capability flag: there is no offline client
                // in this codebase, so offline queueing and sync are
                // not supported server-side here. A future client may
                // queue sales and replay them through the normal sale
                // endpoints; until then this stays "not_supported".
                "offline_queueing": "not_supported",
                "split_payment": settings.allow_split_payment,
                "receipt_print_lines": true,
            },
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.settings();
        if let Some(store_name) = &input.store_name {
            settings.store_name = require_text(store_name, "store_name", MAX_NAME_LEN)?;
        }
        if let Some(store_address) = &input.store_address {
            settings.store_address = match store_address {
                Some(text) => Some(require_text(text, "store_address", MAX_NAME_LEN)?),
                None => None,
            };
        }
        if let Some(receipt_footer) = &input.receipt_footer {
            settings.receipt_footer = match receipt_footer {
                Some(text) => Some(require_text(text, "receipt_footer", MAX_NOTES_LEN)?),
                None => None,
            };
        }
        if let Some(width) = input.receipt_width_chars {
            if width < 16 || width > MAX_RECEIPT_WIDTH {
                return Err(UnifiedError::bad_request(format!(
                    "receipt_width_chars must be between 16 and {MAX_RECEIPT_WIDTH}"
                )));
            }
            settings.receipt_width_chars = width;
        }
        if let Some(currency) = &input.default_currency {
            settings.default_currency = valid_currency(currency)?;
        }
        if let Some(default_tax_rate) = input.default_tax_rate {
            if !default_tax_rate.is_finite() || default_tax_rate < 0.0 || default_tax_rate > 10.0 {
                return Err(UnifiedError::bad_request(
                    "default_tax_rate must be a finite fraction between 0 and 10",
                ));
            }
            settings.default_tax_rate = default_tax_rate;
        }
        if let Some(tax_inclusive) = input.tax_inclusive {
            settings.tax_inclusive = tax_inclusive;
        }
        if let Some(max_discount_percent) = input.max_discount_percent {
            if !max_discount_percent.is_finite()
                || max_discount_percent < 0.0
                || max_discount_percent > MAX_DISCOUNT_PERCENT
            {
                return Err(UnifiedError::bad_request(
                    "max_discount_percent must be between 0 and 100",
                ));
            }
            settings.max_discount_percent = max_discount_percent;
        }
        if let Some(require_cashier_assignment) = input.require_cashier_assignment {
            settings.require_cashier_assignment = require_cashier_assignment;
        }
        if let Some(allow_split_payment) = input.allow_split_payment {
            settings.allow_split_payment = allow_split_payment;
        }
        *state
            .settings
            .write()
            .expect("pos settings lock poisoned") = settings.clone();
        state.emit(
            "pos.settings.updated",
            json!({
                "updated_by": caller.actor,
                "default_currency": settings.default_currency,
                "default_tax_rate": settings.default_tax_rate,
            }),
        );
        Ok(settings)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// End-of-day report
// ---------------------------------------------------------------------------

async fn end_of_day_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    query: web::Query<ReportQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        let input = query.into_inner();
        let date = match &input.date {
            Some(raw) => NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d").map_err(|_| {
                UnifiedError::bad_request("date must be YYYY-MM-DD")
            })?,
            None => Utc::now().date_naive(),
        };
        let day_start = date.and_hms_opt(0, 0, 0).unwrap().and_utc();
        let day_end = (date + chrono::Duration::days(1))
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        let register_filter = input.register_id.clone();
        let mut gross_sales = 0.0;
        let mut discount_total = 0.0;
        let mut tax_total = 0.0;
        let mut sales_count = 0u64;
        let mut voids_count = 0u64;
        let mut refunds_total = 0.0;
        let mut refunds_count = 0u64;
        let mut by_method: std::collections::HashMap<String, f64> =
            std::collections::HashMap::new();
        let mut by_register: std::collections::HashMap<String, (f64, u64)> =
            std::collections::HashMap::new();
        for entry in state.sales.iter() {
            let sale = entry.value();
            if let Some(register_id) = &register_filter {
                if sale.register_id != *register_id {
                    continue;
                }
            }
            if sale.status == "voided" {
                if sale.voided_at.map(|at| at >= day_start && at < day_end).unwrap_or(false) {
                    voids_count += 1;
                }
                continue;
            }
            let Some(completed_at) = sale.completed_at else {
                continue;
            };
            if completed_at < day_start || completed_at >= day_end {
                continue;
            }
            if !matches!(
                sale.status.as_str(),
                "completed" | "partially_refunded" | "refunded"
            ) {
                continue;
            }
            sales_count += 1;
            gross_sales += sale.total;
            discount_total += sale.discount_total;
            tax_total += sale.tax_total;
            let register_entry = by_register
                .entry(sale.register_id.clone())
                .or_insert((0.0, 0));
            register_entry.0 += sale.total;
            register_entry.1 += 1;
            for payment in state.payments.iter() {
                let payment = payment.value();
                if payment.sale_id == sale.id {
                    *by_method.entry(payment.method_code.clone()).or_insert(0.0) += payment.amount;
                }
            }
        }
        for entry in state.refunds.iter() {
            let refund = entry.value();
            if refund.created_at < day_start || refund.created_at >= day_end {
                continue;
            }
            if let Some(register_id) = &register_filter {
                let sale = state.sales.get(&refund.sale_id);
                if sale.map(|sale| sale.value().register_id != *register_id).unwrap_or(true) {
                    continue;
                }
            }
            refunds_total += refund.amount;
            refunds_count += 1;
        }
        let mut by_method_json: Vec<Value> = by_method
            .into_iter()
            .map(|(method, amount)| json!({ "method": method, "amount": round_money(amount) }))
            .collect();
        by_method_json.sort_by(|a, b| {
            a.get("method")
                .and_then(Value::as_str)
                .cmp(&b.get("method").and_then(Value::as_str))
        });
        let mut by_register_json: Vec<Value> = by_register
            .into_iter()
            .map(|(register_id, (total, count))| {
                json!({
                    "register_id": register_id,
                    "gross_sales": round_money(total),
                    "sales_count": count,
                })
            })
            .collect();
        by_register_json.sort_by(|a, b| {
            a.get("register_id")
                .and_then(Value::as_str)
                .cmp(&b.get("register_id").and_then(Value::as_str))
        });
        // Shift cash variances for shifts closed on this date.
        let mut variances: Vec<Value> = Vec::new();
        for entry in state.shifts.iter() {
            let shift = entry.value();
            if let Some(register_id) = &register_filter {
                if shift.register_id != *register_id {
                    continue;
                }
            }
            if shift.status != "closed" {
                continue;
            }
            let Some(closed_at) = shift.closed_at else {
                continue;
            };
            if closed_at < day_start || closed_at >= day_end {
                continue;
            }
            variances.push(json!({
                "shift_id": shift.id,
                "register_id": shift.register_id,
                "cashier_id": shift.cashier_id,
                "opening_float": shift.opening_float,
                "expected_cash": shift.expected_cash,
                "counted_cash": shift.counted_cash,
                "variance": shift.variance,
                "currency": shift.currency,
            }));
        }
        variances.sort_by(|a, b| {
            a.get("register_id")
                .and_then(Value::as_str)
                .cmp(&b.get("register_id").and_then(Value::as_str))
        });
        Ok(json!({
            "date": date.to_string(),
            "register_id": register_filter,
            "sales_count": sales_count,
            "gross_sales": round_money(gross_sales),
            "discount_total": round_money(discount_total),
            "tax_total": round_money(tax_total),
            "refunds_total": round_money(refunds_total),
            "refunds_count": refunds_count,
            "voids_count": voids_count,
            "net_sales": round_money(gross_sales - refunds_total),
            "by_payment_method": by_method_json,
            "by_register": by_register_json,
            "shift_variances": variances,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
