// Pos module — register, shift, sale, receipt, and settings types.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
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
use super::*;

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
    /// Opening float in minor units of `currency`. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub opening_float: i64,
    pub currency: String,
    /// Cash the drawer should hold at close (computed at close).
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub expected_cash: Option<i64>,
    /// Cash actually counted at close.
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub counted_cash: Option<i64>,
    /// `counted_cash - expected_cash` (computed at close).
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub variance: Option<i64>,
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
    /// Unit price in minor units. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub unit_price: i64,
    pub quantity: u32,
    /// Quantity already refunded on this line.
    #[serde(default)]
    pub refunded_quantity: u32,
    /// `percent` or `fixed` (empty means no line discount).
    #[serde(default)]
    pub discount_type: Option<String>,
    /// Percent (0..=100) or fixed amount in major units of the sale
    /// currency. Stays `f64` by design: a percent cannot be minor
    /// units; fixed amounts convert to minor units at computation
    /// time in `discount_amount`.
    #[serde(default)]
    pub discount_value: f64,
    /// Discount amount for the whole line, in minor units. JSON carries major units.
    #[serde(default, with = "unified_core::money::serde_major")]
    pub discount_amount: i64,
    /// Net line amount after discounts, before tax, in minor units. JSON carries major units.
    #[serde(default, with = "unified_core::money::serde_major")]
    pub line_subtotal: i64,
    /// Tax rate applied to this line, as a fraction.
    #[serde(default)]
    pub tax_rate: f64,
    /// Tax amount for this line, in minor units. JSON carries major units.
    #[serde(default, with = "unified_core::money::serde_major")]
    pub tax_amount: i64,
    /// Line total including tax, in minor units. JSON carries major units.
    #[serde(default, with = "unified_core::money::serde_major")]
    pub line_total: i64,
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
    /// Payment amount in minor units. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub amount: i64,
    /// Amount tendered by the customer; defaults to `amount`.
    #[serde(default, with = "unified_core::money::serde_major")]
    pub tendered: i64,
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
    /// Net amount in minor units: sum of the net line amounts after line discounts. JSON carries major units.
    #[serde(default, with = "unified_core::money::serde_major")]
    pub subtotal: i64,
    /// Sale-level discount type: `percent`, `fixed`, or empty.
    #[serde(default)]
    pub discount_type: Option<String>,
    /// Sale-level discount value: percent (0..=100) or fixed amount
    /// in major units. Stays `f64` by design (see `PosSaleItem::discount_value`).
    #[serde(default)]
    pub discount_value: f64,
    /// Sale-level discount amount, in minor units. JSON carries major units.
    #[serde(default, with = "unified_core::money::serde_major")]
    pub discount_total: i64,
    /// Total tax across all lines, in minor units. JSON carries major units.
    #[serde(default, with = "unified_core::money::serde_major")]
    pub tax_total: i64,
    /// Total amount due, including tax, in minor units. JSON carries major units.
    #[serde(default, with = "unified_core::money::serde_major")]
    pub total: i64,
    pub currency: String,
    /// Whether line prices include tax.
    #[serde(default)]
    pub tax_inclusive: bool,
    /// `draft`, `held`, `completed`, `voided`,
    /// `partially_refunded`, or `refunded`.
    pub status: String,
    /// Amount paid so far across all recorded payments, in minor units. JSON carries major units.
    #[serde(default, with = "unified_core::money::serde_major")]
    pub amount_paid: i64,
    /// Change handed back to the customer at completion, in minor units. JSON carries major units.
    #[serde(default, with = "unified_core::money::serde_major")]
    pub change_due: i64,
    /// Amount refunded so far across all refunds, in minor units. JSON carries major units.
    #[serde(default, with = "unified_core::money::serde_major")]
    pub refunded_amount: i64,
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
    /// Refund amount in minor units. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub amount: i64,
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
    /// Refund amount for this line, in minor units. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub amount: i64,
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
    #[serde(with = "unified_core::money::serde_major")]
    pub subtotal: i64,
    #[serde(with = "unified_core::money::serde_major")]
    pub discount_total: i64,
    #[serde(with = "unified_core::money::serde_major")]
    pub tax_total: i64,
    #[serde(with = "unified_core::money::serde_major")]
    pub total: i64,
    pub currency: String,
    /// Tax grouped by rate, for the receipt tax breakdown.
    #[serde(default)]
    pub tax_breakdown: Vec<TaxBreakdownLine>,
    pub payments: Vec<PosPayment>,
    #[serde(with = "unified_core::money::serde_major")]
    pub amount_paid: i64,
    #[serde(with = "unified_core::money::serde_major")]
    pub change_due: i64,
    pub print_lines: Vec<ReceiptPrintLine>,
    #[serde(default)]
    pub footer: Option<String>,
    pub issued_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxBreakdownLine {
    pub tax_rate: f64,
    /// Tax amount in minor units. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub tax_amount: i64,
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
    /// Two-letter ISO country code of the store, when configured.
    /// Included as `country_code` in the emitted `order.created`
    /// payload so the tax module can pick the right jurisdiction.
    #[serde(default)]
    pub country_code: Option<String>,
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
            country_code: None,
            default_tax_rate: 0.0,
            tax_inclusive: false,
            max_discount_percent: 100.0,
            require_cashier_assignment: true,
            allow_split_payment: true,
        }
    }
}
