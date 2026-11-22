// Billing / Invoicing module.
//
// Invoices built from orders (directly or automatically from the
// `order.created` hook), payment and voiding, structured rendering, and
// overdue detection. Interconnection happens only through the hook bus.
//
// Invoice numbering uses a configurable prefix plus a zero-padded
// sequence that is never reused. Taxes are computed per line from a
// configurable default rate, with tax-inclusive or tax-exclusive
// handling. Due dates default to the issue date plus the configured
// payment terms; overdue status is derived from the due date and the
// outstanding balance. Overdue reminders are emitted as the
// `billing.invoice.reminder` hook for the mail module to deliver -
// this module never sends mail itself. Credit notes reduce an
// invoice's balance, payments are recorded per invoice (partial
// payments included), and `/pdf-data` returns the full structured
// invoice payload shaped for a PDF renderer.
//
// Subscription lifecycle: the store owns the authoritative
// `Subscription` record and emits `subscription.created` when a
// subscription order is paid. Billing keeps a mirror
// (`SubscriptionLifecycle`) and runs the lifecycle engine over it:
// renewal invoices generated before the period ends, proration
// invoices from `subscription.upgraded` / `subscription.downgraded`,
// dunning on `payment.failed` (retries per the product's dunning
// settings, `notification.requested` on each failure and on final
// failure), renewal reminders before the period ends, trial-end
// transitions, and grace-period expiry. Billing never writes the
// store's records directly: every decision is applied through
// `subscription.lifecycle.apply`, which the store applies and
// answers with the public `subscription.*` event. Renewal charges
// are requested as `subscription.renewal_due` (the payments module
// owns the gateway charge; see the integration notes for the
// convergence point) and confirmed through `payment.captured`.
//
// Invoice templates and printing (batch 12): invoice templates
// are data, not hard-coded layouts (header/footer, logo storage
// reference, colours, columns, line-item layout, tax breakdown,
// payment instructions, footer notes, per-company templates with
// version history and restore, preview with sample data). Printing
// an invoice renders a print-ready HTML document with `@page`
// rules for the chosen paper size and queues the job through the
// printing module over the `printing.job.requested` event, then
// emits `billing.invoice.printed`; emailing an invoice sends the
// same document through `mail.send_requested` (the mail module has
// no attachment field, so the document travels as the HTML body).
// Binary PDF conversion is a deployment step, not a claim made
// here. Paper sizes are owned by the printing module; billing
// only names the size (or the explicit millimetre dimensions)
// the document was rendered for.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::reference_id::{self, ReferenceIdSettings};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvoiceItem {
    pub name: String,
    pub quantity: u32,
    pub unit_price: f64,
    pub line_total: f64,
    /// Tax rate applied to this line, as a fraction (0.10 means 10%).
    #[serde(default)]
    pub tax_rate: f64,
    /// Tax amount for this line. `line_total` is the net amount when
    /// tax is exclusive and the gross amount when tax is inclusive;
    /// either way `subtotal + tax_total` equals the invoice total.
    #[serde(default)]
    pub tax_amount: f64,
}

/// Billing configuration. Kept in memory in this module; the durable
/// form is the `billing_settings` table described in the migration
/// snippet reported to the coordinator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingSettings {
    /// Prefix for generated invoice numbers, for example `INV`.
    pub invoice_prefix: String,
    /// Default per-line tax rate as a fraction (0.10 means 10%).
    pub default_tax_rate: f64,
    /// When true, line prices already include tax; otherwise tax is
    /// added on top of the net line amounts.
    pub tax_inclusive: bool,
    /// Payment terms in days: the due date defaults to the issue date
    /// plus this many days.
    pub payment_terms_days: u32,
    /// Reminder schedule: days before the due date a reminder is due
    /// (for example `[7, 3, 1]`).
    pub reminder_before_days: Vec<i32>,
    /// Reminder schedule: days after the due date a reminder is due
    /// (for example `[1, 7, 14]`).
    pub reminder_after_days: Vec<i32>,
}

impl Default for BillingSettings {
    fn default() -> Self {
        Self {
            invoice_prefix: "INV".to_string(),
            default_tax_rate: 0.0,
            tax_inclusive: false,
            payment_terms_days: 30,
            reminder_before_days: vec![7, 3, 1],
            reminder_after_days: vec![1, 7, 14],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invoice {
    pub id: String,
    /// Human-readable invoice number, for example `INV-000001`.
    /// Unique, never reused; the sequence is monotonic.
    #[serde(default)]
    pub number: String,
    /// Public payment reference for this invoice, in the single
    /// shared reference format (`PREFIX-12hex-12hex`, 30 characters;
    /// see `crate::core::reference_id`). An invoice built from an
    /// order reuses that order's reference so the gateway sees one
    /// reference end to end; any other invoice gets a fresh
    /// reference at creation. Issued once and never changed.
    /// `#[serde(default)]` keeps invoices serialized before this
    /// field existed readable; such invoices carry an empty
    /// reference until the integrator backfills them.
    #[serde(default)]
    pub reference: String,
    pub order_id: Option<String>,
    /// Commercial subscription this invoice belongs to, when it is a
    /// renewal or proration invoice. The initial order invoice has
    /// no subscription id: the subscription only exists once the
    /// order is paid.
    #[serde(default)]
    pub subscription_id: Option<String>,
    pub user_id: String,
    pub company_id: Option<String>,
    pub items: Vec<InvoiceItem>,
    /// Net amount: the sum of the net line amounts.
    #[serde(default)]
    pub subtotal: f64,
    /// Total tax across all lines.
    #[serde(default)]
    pub tax_total: f64,
    pub total: f64,
    pub currency: String,
    /// Base status: `draft`, `issued`, `paid`, `partially_paid`,
    /// `unpaid`, `void`, `credited`. The serialized status is the
    /// effective status: an unpaid invoice past its due date reads
    /// as `overdue` (see `view_invoice`).
    pub status: String,
    /// True when the invoice is past its due date with a balance
    /// outstanding. Derived on read, not stored as source of truth.
    #[serde(default)]
    pub overdue: bool,
    /// Whether line prices include tax.
    #[serde(default)]
    pub tax_inclusive: bool,
    /// Amount paid so far across all recorded payments.
    #[serde(default)]
    pub amount_paid: f64,
    /// Amount credited so far across all credit notes.
    #[serde(default)]
    pub credit_total: f64,
    /// Outstanding amount: `total - amount_paid - credit_total`.
    #[serde(default)]
    pub balance: f64,
    /// Date the invoice was issued (the creation date).
    #[serde(default)]
    pub issue_date: Option<DateTime<Utc>>,
    pub due_date: Option<DateTime<Utc>>,
    /// Payment terms in days that produced the due date.
    #[serde(default)]
    pub payment_terms_days: Option<u32>,
    pub paid_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub notes: Option<String>,
    /// Reminder offsets already sent, in days relative to the due
    /// date: positive means days before due, negative means days
    /// after due (for example `7` and `-1`).
    #[serde(default)]
    pub reminders_sent: Vec<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One payment recorded against an invoice. Partial payments are
/// normal: each one updates the invoice balance and status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvoicePayment {
    pub id: String,
    pub invoice_id: String,
    pub amount: f64,
    pub method: String,
    #[serde(default)]
    pub reference: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

/// One expense-reimbursement disbursement recorded by billing.
/// Billing holds no gateway payout integration, so the disbursement
/// is recorded here in billing's own ledger as the payment of
/// record — the same manual-disbursement model as `pay_invoice` —
/// and confirmed to expenses with `expenses.reimbursement.paid`.
/// A future gateway payout call would replace the record-and-confirm
/// step with a real transfer and only then emit `paid`. Amounts are
/// integer minor units, matching the expenses module's contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReimbursementPayout {
    /// The expenses reimbursement id this disbursement pays.
    pub id: String,
    #[serde(default)]
    pub report_id: Option<String>,
    #[serde(default)]
    pub employee_id: Option<String>,
    pub amount_minor: i64,
    pub currency: String,
    /// `paid` or `failed`.
    pub status: String,
    #[serde(default)]
    pub failure_reason: Option<String>,
    #[serde(default)]
    pub paid_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub attempts: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One digital-product royalty payout recorded by billing. The
/// store emits `store.digital.payout.requested` when a payout is
/// requested and `store.digital.payout.approved` when a manager
/// approves it; billing records the obligation on request and the
/// disbursement on approval (the store's mark-paid endpoint stays
/// the step that flips the store-side record). Amounts are major
/// units, matching the store's `DigitalPayoutRequest`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingDigitalPayout {
    /// The store payout request id.
    pub id: String,
    #[serde(default)]
    pub payee: Option<String>,
    pub amount: f64,
    pub currency: String,
    #[serde(default)]
    pub royalty_ids: Vec<String>,
    /// `requested` or `paid`.
    pub status: String,
    #[serde(default)]
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A credit note issued against an invoice. It reduces the invoice
/// balance; when the balance reaches zero the invoice reads as
/// `credited`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreditNote {
    pub id: String,
    pub invoice_id: String,
    /// Human-readable credit note number, for example `CN-000001`.
    pub number: String,
    pub amount: f64,
    pub reason: String,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Subscription lifecycle (mirror of the store's authoritative record)
// ---------------------------------------------------------------------------

/// Days before the period ends when the renewal invoice is
/// generated.
pub const RENEWAL_INVOICE_LEAD_DAYS: i64 = 2;

/// Days before the period ends when the renewal reminder is sent.
pub const RENEWAL_REMINDER_LEAD_DAYS: i64 = 3;

/// Billing's mirror of one store subscription: the commercial fields
/// synced from `subscription.created` and every later
/// `subscription.*` event, plus the lifecycle bookkeeping only
/// billing needs (renewal invoice, reminder, dunning counters).
/// The store record stays authoritative; this mirror never drives a
/// status change on its own — decisions go out as
/// `subscription.lifecycle.apply` and come back as events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionLifecycle {
    pub subscription_id: String,
    pub user_id: String,
    #[serde(default)]
    pub company_id: Option<String>,
    pub product_id: String,
    #[serde(default)]
    pub module_id: Option<String>,
    pub order_id: String,
    /// `trialing`, `active`, `past_due`, or `cancelled`.
    pub status: String,
    pub billing_period: String,
    /// Price charged per period, in major units.
    pub price: f64,
    pub currency: String,
    #[serde(default)]
    pub trial_days: u32,
    #[serde(default)]
    pub trial_end: Option<DateTime<Utc>>,
    pub current_period_start: DateTime<Utc>,
    pub current_period_end: DateTime<Utc>,
    pub proration_policy: String,
    pub grace_period_days: u32,
    pub dunning_max_retries: u32,
    pub dunning_retry_interval_days: u32,
    #[serde(default)]
    pub cancel_at_period_end: bool,
    #[serde(default)]
    pub cancelled_at: Option<DateTime<Utc>>,
    /// Renewal invoice issued for the current period end, if any.
    #[serde(default)]
    pub renewal_invoice_id: Option<String>,
    /// Period end the renewal invoice above was issued for, so the
    /// scan issues exactly one renewal invoice per period.
    #[serde(default)]
    pub renewal_invoice_for_period_end: Option<DateTime<Utc>>,
    /// Period end a renewal reminder was already sent for, so the
    /// scan reminds once per period.
    #[serde(default)]
    pub renewal_reminder_sent_for: Option<DateTime<Utc>>,
    /// Failed renewal payments recorded for the current dunning run.
    #[serde(default)]
    pub dunning_attempts: u32,
    #[serde(default)]
    pub last_failure_at: Option<DateTime<Utc>>,
    /// Earliest time the next dunning retry may be requested.
    #[serde(default)]
    pub next_retry_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One recorded failed renewal payment (dunning attempt). Each
/// failure is recorded, whether or not a retry follows, so the
/// dunning history survives the subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DunningAttempt {
    pub id: String,
    pub subscription_id: String,
    /// 1-based attempt number within the current dunning run.
    pub attempt_number: u32,
    pub reason: String,
    #[serde(default)]
    pub amount_minor: Option<i64>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateInvoice {
    pub order_id: Option<String>,
    pub user_id: String,
    pub company_id: Option<String>,
    pub items: Vec<InvoiceItemInput>,
    pub total: Option<f64>,
    pub currency: Option<String>,
    pub due_date: Option<DateTime<Utc>>,
    /// Payment terms in days; defaults to the configured terms.
    #[serde(default)]
    pub payment_terms_days: Option<u32>,
    /// When set, overrides the configured tax-inclusive flag.
    #[serde(default)]
    pub tax_inclusive: Option<bool>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InvoiceItemInput {
    pub name: String,
    pub quantity: Option<u32>,
    pub unit_price: f64,
    /// Tax rate for this line as a fraction (0.10 means 10%).
    /// Defaults to the configured default tax rate.
    #[serde(default)]
    pub tax_rate: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateInvoice {
    pub status: Option<String>,
    pub due_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateBillingSettings {
    pub invoice_prefix: Option<String>,
    pub default_tax_rate: Option<f64>,
    pub tax_inclusive: Option<bool>,
    pub payment_terms_days: Option<u32>,
    pub reminder_before_days: Option<Vec<i32>>,
    pub reminder_after_days: Option<Vec<i32>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecordPaymentRequest {
    pub amount: f64,
    pub method: Option<String>,
    #[serde(default)]
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateCreditNoteRequest {
    pub amount: f64,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemindRequest {
    /// Reminder offset in days relative to the due date (positive =
    /// days before due, negative = days after due). Defaults to the
    /// offset for today, computed from the due date.
    #[serde(default)]
    pub offset_days: Option<i32>,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FromOrderRequest {
    pub order_id: String,
    pub user_id: Option<String>,
    pub due_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PayInvoiceRequest {
    pub method: Option<String>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct BillingState {
    hooks: RwLock<Arc<HookBus>>,
    invoices: DashMap<String, Invoice>,
    /// Orders seen through the `order.created` hook, keyed by order id,
    /// so `from-order` can build an invoice from real order data.
    seen_orders: DashMap<String, Value>,
    payments: DashMap<String, InvoicePayment>,
    credit_notes: DashMap<String, CreditNote>,
    settings: RwLock<BillingSettings>,
    /// Admin-editable payment reference settings (default prefix and
    /// per-product prefix map; see `crate::core::reference_id`).
    /// Fresh invoice references are issued from these settings;
    /// order-derived invoices reuse the order's reference instead.
    /// Changed through `GET/PUT /api/v1/billing/reference-settings`.
    reference_settings: RwLock<ReferenceIdSettings>,
    /// Monotonic invoice number sequence. Never reused and never
    /// reset while the process runs; the durable form is the
    /// `invoice_sequences` table in the migration snippet reported to
    /// the coordinator.
    invoice_seq: AtomicU64,
    /// Monotonic credit note number sequence.
    credit_seq: AtomicU64,
    /// Payment events already applied (`capture:<id>` /
    /// `refund:<id>`), so a redelivered `payment.captured` or
    /// `payment.refunded` never records the same payment or credit
    /// twice. Entries are keyed by the provider refund / transaction
    /// id when the event carries one, otherwise by order id.
    processed_payment_events: DashMap<String, DateTime<Utc>>,
    /// Subscription lifecycle mirrors, keyed by subscription id.
    /// Synced from the store's `subscription.*` events.
    subscription_lifecycles: DashMap<String, SubscriptionLifecycle>,
    /// Recorded failed renewal payments, keyed by attempt id.
    dunning_attempts: DashMap<String, DunningAttempt>,
    /// Expense-reimbursement disbursements recorded by billing,
    /// keyed by reimbursement id. See `ReimbursementPayout`.
    reimbursement_payouts: DashMap<String, ReimbursementPayout>,
    /// Digital-product royalty payouts recorded by billing, keyed
    /// by the store payout request id. See `BillingDigitalPayout`.
    digital_payouts: DashMap<String, BillingDigitalPayout>,
    /// Invoice templates, keyed by template id. Templates are
    /// data: every edit archives the previous version first.
    invoice_templates: DashMap<String, InvoiceTemplate>,
    /// Archived template versions, keyed by version-row id.
    template_versions: DashMap<String, InvoiceTemplateVersion>,
    /// Default invoice template per company, keyed by company id.
    company_default_templates: DashMap<String, String>,
    // --- School fees (see the school-fees section at the end of
    // this file). Money is kept in integer minor units here, never
    // floats; the linked billing invoice is the only place a fee
    // amount is also carried as a major-unit float, converted at
    // the boundary by `minor_to_major` / `major_to_minor`.
    /// Admin-editable fee type definitions, keyed by fee type id.
    fee_types: DashMap<String, SchoolFeeType>,
    /// Per-student fee schedules, keyed by schedule id.
    fee_schedules: DashMap<String, StudentFeeSchedule>,
    /// Generated fee charges, keyed by charge id.
    fee_charges: DashMap<String, FeeCharge>,
    /// Per-student discounts and scholarships, keyed by discount id.
    fee_discounts: DashMap<String, FeeDiscount>,
    /// Issued fee receipts, keyed by receipt id.
    fee_receipts: DashMap<String, FeeReceipt>,
    /// Guardian-to-student links, keyed by link id.
    fee_guardians: DashMap<String, FeeGuardianLink>,
    /// Fee reminders already sent, keyed by
    /// `{charge_id}:{offset_days}`.
    fee_reminders: DashMap<String, FeeReminder>,
    /// School fee settings (configurable defaults).
    fee_settings: RwLock<SchoolFeeSettings>,
    /// Monotonic fee receipt number sequence.
    fee_receipt_seq: AtomicU64,
}

impl BillingState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            invoices: DashMap::new(),
            seen_orders: DashMap::new(),
            payments: DashMap::new(),
            credit_notes: DashMap::new(),
            settings: RwLock::new(BillingSettings::default()),
            reference_settings: RwLock::new(ReferenceIdSettings::default()),
            invoice_seq: AtomicU64::new(1),
            credit_seq: AtomicU64::new(1),
            processed_payment_events: DashMap::new(),
            subscription_lifecycles: DashMap::new(),
            dunning_attempts: DashMap::new(),
            reimbursement_payouts: DashMap::new(),
            digital_payouts: DashMap::new(),
            invoice_templates: DashMap::new(),
            template_versions: DashMap::new(),
            company_default_templates: DashMap::new(),
            fee_types: DashMap::new(),
            fee_schedules: DashMap::new(),
            fee_charges: DashMap::new(),
            fee_discounts: DashMap::new(),
            fee_receipts: DashMap::new(),
            fee_guardians: DashMap::new(),
            fee_reminders: DashMap::new(),
            fee_settings: RwLock::new(SchoolFeeSettings::default()),
            fee_receipt_seq: AtomicU64::new(1),
        }
    }

    fn settings(&self) -> BillingSettings {
        self.settings
            .read()
            .expect("billing settings lock poisoned")
            .clone()
    }

    /// Next invoice number: configured prefix plus a zero-padded
    /// sequence that is never reused.
    fn next_invoice_number(&self) -> String {
        let prefix = self.settings().invoice_prefix;
        let seq = self.invoice_seq.fetch_add(1, Ordering::SeqCst);
        format!("{prefix}-{seq:06}")
    }

    fn next_credit_note_number(&self) -> String {
        let seq = self.credit_seq.fetch_add(1, Ordering::SeqCst);
        format!("CN-{seq:06}")
    }

    /// Preview of the next invoice number without consuming it.
    fn next_invoice_number_preview(&self) -> String {
        let prefix = self.settings().invoice_prefix;
        let seq = self.invoice_seq.load(Ordering::SeqCst);
        format!("{prefix}-{seq:06}")
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Build a draft invoice from an order payload (as emitted by the
    /// store on `order.created`). Returns `None` when the payload lacks
    /// the fields an invoice needs. Order line amounts are treated as
    /// net amounts; tax is computed from the configured default rate
    /// and tax-inclusive flag.
    fn invoice_from_order_value(&self, order: &Value) -> Option<Invoice> {
        let order_id = order.get("id")?.as_str()?.to_string();
        let user_id = order.get("user_id")?.as_str()?.to_string();
        let total = order.get("total")?.as_f64()?;
        let currency = order
            .get("currency")
            .and_then(|v| v.as_str())
            .unwrap_or("USD")
            .to_string();
        let settings = self.settings();
        let created = now();
        // Order line totals are gross: the coupon discount lives on
        // the order (`discount_total`), not on the lines. Allocating
        // the discount across the lines in proportion to their gross
        // amounts — with the rounding remainder on the largest line —
        // keeps the lines reconciled to the order total; without it
        // the lines sum to the pre-coupon subtotal while the amount
        // due is net.
        let discount_total = order
            .get("discount_total")
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0)
            .max(0.0);
        let mut raw_lines: Vec<(String, u32, f64, f64)> = Vec::new();
        if let Some(raw_items) = order.get("items").and_then(|v| v.as_array()) {
            for raw in raw_items {
                let name = raw
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Item")
                    .to_string();
                let quantity = raw
                    .get("quantity")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1) as u32;
                let unit_price = raw.get("unit_price").and_then(|v| v.as_f64()).unwrap_or(0.0);
                // Prefer the order's own line total; fall back to unit
                // price times quantity.
                let gross = raw
                    .get("line_total")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(unit_price * quantity as f64)
                    .max(0.0);
                raw_lines.push((name, quantity, unit_price, gross));
            }
        }
        let gross_sum: f64 = raw_lines.iter().map(|line| line.3).sum();
        let mut allocations: Vec<f64> = vec![0.0; raw_lines.len()];
        if discount_total > 0.0 && gross_sum > 0.0 {
            let discount = discount_total.min(gross_sum);
            let mut allocated_sum = 0.0;
            for (index, line) in raw_lines.iter().enumerate() {
                let share = round_money(discount * line.3 / gross_sum);
                allocations[index] = share.min(line.3);
                allocated_sum += allocations[index];
            }
            // The rounding remainder goes on the largest line so the
            // allocations sum to exactly the discount.
            let remainder = round_money(discount - allocated_sum);
            if remainder != 0.0 {
                if let Some((largest, _)) = raw_lines
                    .iter()
                    .enumerate()
                    .max_by(|(_, a), (_, b)| {
                        a.3.partial_cmp(&b.3)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                {
                    allocations[largest] =
                        round_money(allocations[largest] + remainder).clamp(0.0, raw_lines[largest].3);
                }
            }
        }
        let mut items: Vec<InvoiceItem> = Vec::new();
        let mut subtotal = 0.0;
        let mut tax_total = 0.0;
        for (index, (name, quantity, unit_price, gross)) in
            raw_lines.into_iter().enumerate()
        {
            let base_amount = round_money(gross - allocations[index]).max(0.0);
            let (net, tax) = line_tax_on_amount(
                base_amount,
                settings.default_tax_rate,
                settings.tax_inclusive,
            );
            let line_total = if settings.tax_inclusive {
                round_money(net + tax)
            } else {
                net
            };
            subtotal += net;
            tax_total += tax;
            items.push(InvoiceItem {
                name,
                quantity,
                unit_price: round_money(unit_price),
                line_total,
                tax_rate: settings.default_tax_rate,
                tax_amount: tax,
            });
        }
        if items.is_empty() {
            let (net, tax) = line_tax(
                total,
                1,
                settings.default_tax_rate,
                settings.tax_inclusive,
            );
            let line_total = if settings.tax_inclusive {
                round_money(net + tax)
            } else {
                net
            };
            subtotal += net;
            tax_total += tax;
            items.push(InvoiceItem {
                name: "Order".to_string(),
                quantity: 1,
                unit_price: round_money(total),
                line_total,
                tax_rate: settings.default_tax_rate,
                tax_amount: tax,
            });
        }
        let computed_total = round_money(subtotal + tax_total);
        Some(Invoice {
            id: new_id(),
            number: self.next_invoice_number(),
            reference: invoice_reference_from_order(self, order),
            order_id: Some(order_id),
            subscription_id: None,
            user_id,
            company_id: order
                .get("company_id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            items,
            subtotal: round_money(subtotal),
            tax_total: round_money(tax_total),
            total: computed_total,
            currency,
            status: "draft".to_string(),
            overdue: false,
            tax_inclusive: settings.tax_inclusive,
            amount_paid: 0.0,
            credit_total: 0.0,
            balance: computed_total,
            issue_date: Some(created),
            due_date: Some(created + Duration::days(i64::from(settings.payment_terms_days))),
            payment_terms_days: Some(settings.payment_terms_days),
            paid_at: None,
            notes: None,
            reminders_sent: Vec::new(),
            created_at: created,
            updated_at: created,
        })
    }

    /// Length of one billing period in days, using the same
    /// convention as the store: weekly 7, monthly 30, yearly 365.
    fn period_days(billing_period: &str) -> i64 {
        match billing_period {
            "weekly" => 7,
            "yearly" => 365,
            _ => 30,
        }
    }

    /// Build (or refresh) the lifecycle mirror from a store
    /// subscription payload — either the subscription object itself
    /// (as emitted by `subscription.created`) or an event payload
    /// carrying it under `"subscription"`. Commercial fields are
    /// always overwritten from the store record; lifecycle
    /// bookkeeping (renewal invoice, reminder, dunning counters)
    /// survives the sync, except that a new period clears the
    /// per-period bookkeeping. Returns `None` when the payload
    /// lacks the fields a mirror needs.
    fn sync_subscription_lifecycle(&self, value: &Value) -> Option<SubscriptionLifecycle> {
        let subscription = value.get("subscription").unwrap_or(value);
        let id = subscription.get("id")?.as_str()?.to_string();
        let parse_date = |key: &str| {
            subscription
                .get(key)
                .and_then(|value| value.as_str())
                .and_then(|text| text.parse::<DateTime<Utc>>().ok())
        };
        let current_period_start = parse_date("current_period_start")?;
        let current_period_end = parse_date("current_period_end")?;
        let existing = self
            .subscription_lifecycles
            .get(&id)
            .map(|entry| entry.value().clone());
        let mut mirror = SubscriptionLifecycle {
            subscription_id: id,
            user_id: subscription
                .get("user_id")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string(),
            company_id: subscription
                .get("company_id")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            product_id: subscription
                .get("product_id")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string(),
            module_id: subscription
                .get("module_id")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            order_id: subscription
                .get("order_id")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string(),
            status: subscription
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("active")
                .to_string(),
            billing_period: subscription
                .get("billing_period")
                .and_then(|value| value.as_str())
                .unwrap_or("monthly")
                .to_string(),
            price: subscription
                .get("price")
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0),
            currency: subscription
                .get("currency")
                .and_then(|value| value.as_str())
                .unwrap_or("USD")
                .to_string(),
            trial_days: subscription
                .get("trial_days")
                .and_then(|value| value.as_u64())
                .unwrap_or(0) as u32,
            trial_end: parse_date("trial_end"),
            current_period_start,
            current_period_end,
            proration_policy: subscription
                .get("proration_policy")
                .and_then(|value| value.as_str())
                .unwrap_or("prorate")
                .to_string(),
            grace_period_days: subscription
                .get("grace_period_days")
                .and_then(|value| value.as_u64())
                .unwrap_or(0) as u32,
            dunning_max_retries: subscription
                .get("dunning_max_retries")
                .and_then(|value| value.as_u64())
                .unwrap_or(3) as u32,
            dunning_retry_interval_days: subscription
                .get("dunning_retry_interval_days")
                .and_then(|value| value.as_u64())
                .unwrap_or(3) as u32,
            cancel_at_period_end: subscription
                .get("cancel_at_period_end")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            cancelled_at: parse_date("cancelled_at"),
            renewal_invoice_id: existing
                .as_ref()
                .and_then(|mirror| mirror.renewal_invoice_id.clone()),
            renewal_invoice_for_period_end: existing
                .as_ref()
                .and_then(|mirror| mirror.renewal_invoice_for_period_end),
            renewal_reminder_sent_for: existing
                .as_ref()
                .and_then(|mirror| mirror.renewal_reminder_sent_for),
            dunning_attempts: existing
                .as_ref()
                .map(|mirror| mirror.dunning_attempts)
                .unwrap_or(0),
            last_failure_at: existing.as_ref().and_then(|mirror| mirror.last_failure_at),
            next_retry_at: existing.as_ref().and_then(|mirror| mirror.next_retry_at),
            created_at: existing
                .as_ref()
                .map(|mirror| mirror.created_at)
                .unwrap_or_else(now),
            updated_at: now(),
        };
        // A new period clears the per-period bookkeeping: the next
        // period gets a fresh renewal invoice, reminder, and dunning
        // run.
        if let Some(previous) = &existing {
            if previous.current_period_end != mirror.current_period_end {
                mirror.renewal_invoice_id = None;
                mirror.renewal_invoice_for_period_end = None;
                mirror.renewal_reminder_sent_for = None;
                mirror.dunning_attempts = 0;
                mirror.last_failure_at = None;
                mirror.next_retry_at = None;
            }
        }
        self.subscription_lifecycles
            .insert(mirror.subscription_id.clone(), mirror.clone());
        Some(mirror)
    }

    /// Build the renewal invoice for the next period of a
    /// subscription: one line at the recurring price, tax at the
    /// configured default rate, due at the current period end. The
    /// invoice is `issued`, linked to the subscription and the
    /// original order so `payment.captured` can find it.
    fn build_renewal_invoice(&self, mirror: &SubscriptionLifecycle) -> Invoice {
        let settings = self.settings();
        let created = now();
        let (net, tax) = line_tax(
            mirror.price,
            1,
            settings.default_tax_rate,
            settings.tax_inclusive,
        );
        let line_total = if settings.tax_inclusive {
            round_money(net + tax)
        } else {
            net
        };
        let total = round_money(net + tax);
        Invoice {
            id: new_id(),
            number: self.next_invoice_number(),
            reference: fresh_invoice_reference(self),
            order_id: Some(mirror.order_id.clone()),
            subscription_id: Some(mirror.subscription_id.clone()),
            user_id: mirror.user_id.clone(),
            company_id: mirror.company_id.clone(),
            items: vec![InvoiceItem {
                name: format!("Subscription renewal — {}", mirror.product_id),
                quantity: 1,
                unit_price: round_money(mirror.price),
                line_total,
                tax_rate: settings.default_tax_rate,
                tax_amount: tax,
            }],
            subtotal: round_money(net),
            tax_total: round_money(tax),
            total,
            currency: mirror.currency.clone(),
            status: "issued".to_string(),
            overdue: false,
            tax_inclusive: settings.tax_inclusive,
            amount_paid: 0.0,
            credit_total: 0.0,
            balance: total,
            issue_date: Some(created),
            due_date: Some(mirror.current_period_end),
            payment_terms_days: Some(settings.payment_terms_days),
            paid_at: None,
            notes: Some(format!(
                "Renewal for the period ending {}",
                mirror.current_period_end.to_rfc3339()
            )),
            reminders_sent: Vec::new(),
            created_at: created,
            updated_at: created,
        }
    }

    /// Build the proration invoice for a mid-cycle plan change: a
    /// charge line for the new plan and a (negative) credit line for
    /// the unused time on the old plan, both untaxed. When the net
    /// amount is positive the invoice is `issued` and due under the
    /// configured payment terms; when the credit covers the charge
    /// the invoice is recorded as `paid` with nothing due, so the
    /// proration stays on the books without inventing a payment.
    fn build_proration_invoice(
        &self,
        mirror: &SubscriptionLifecycle,
        previous_product_id: &str,
        credit: f64,
        charge: f64,
    ) -> Invoice {
        let settings = self.settings();
        let created = now();
        let mut items: Vec<InvoiceItem> = Vec::new();
        if charge > 0.0 {
            items.push(InvoiceItem {
                name: format!("Plan change charge — {}", mirror.product_id),
                quantity: 1,
                unit_price: round_money(charge),
                line_total: round_money(charge),
                tax_rate: 0.0,
                tax_amount: 0.0,
            });
        }
        if credit > 0.0 {
            items.push(InvoiceItem {
                name: format!("Unused time credit — {previous_product_id}"),
                quantity: 1,
                unit_price: -round_money(credit),
                line_total: -round_money(credit),
                tax_rate: 0.0,
                tax_amount: 0.0,
            });
        }
        let net = round_money(charge - credit);
        let total = net.max(0.0);
        let nothing_due = net <= 0.0;
        Invoice {
            id: new_id(),
            number: self.next_invoice_number(),
            reference: fresh_invoice_reference(self),
            order_id: Some(mirror.order_id.clone()),
            subscription_id: Some(mirror.subscription_id.clone()),
            user_id: mirror.user_id.clone(),
            company_id: mirror.company_id.clone(),
            items,
            subtotal: net,
            tax_total: 0.0,
            total,
            currency: mirror.currency.clone(),
            status: if nothing_due {
                "paid".to_string()
            } else {
                "issued".to_string()
            },
            overdue: false,
            tax_inclusive: settings.tax_inclusive,
            amount_paid: 0.0,
            credit_total: 0.0,
            balance: total,
            issue_date: Some(created),
            due_date: Some(created + Duration::days(i64::from(settings.payment_terms_days))),
            payment_terms_days: Some(settings.payment_terms_days),
            paid_at: if nothing_due { Some(created) } else { None },
            notes: Some(if nothing_due {
                "Proration credit covers the plan change charge; no amount due.".to_string()
            } else {
                "Proration for a mid-cycle plan change.".to_string()
            }),
            reminders_sent: Vec::new(),
            created_at: created,
            updated_at: created,
        }
    }

    /// Request a notification for a subscription owner through the
    /// notifications module (`notification.requested`), the same
    /// direct pattern the licensing module uses for
    /// `license.expiring`: the notifications module does not
    /// subscribe to subscription events, so without this request
    /// the alert would land nowhere.
    fn request_notification(&self, user_id: &str, title: &str, body: &str, kind: &str) {
        self.emit(
            "notification.requested",
            json!({
                "user_id": user_id,
                "title": title,
                "body": body,
                "kind": kind,
            }),
        );
    }

    /// Ask for the renewal charge of a subscription. The scheduler
    /// kind `payment.subscription.renew` is deliberately NOT used:
    /// its payload `subscription_id` names the payments module's own
    /// gateway subscription, not this commercial subscription, so
    /// enqueueing this id would fail there. Instead billing emits
    /// `subscription.renewal_due` naming the order, and the payments
    /// module maps the order to its gateway subscription (see the
    /// integration notes for the convergence point). The charge is
    /// confirmed through `payment.captured`, which advances the
    /// period via `maybe_renew_subscription`.
    fn request_renewal_charge(
        &self,
        mirror: &SubscriptionLifecycle,
        invoice_id: &str,
        retry: bool,
    ) {
        self.emit(
            "subscription.renewal_due",
            json!({
                "subscription_id": mirror.subscription_id,
                "order_id": mirror.order_id,
                "invoice_id": invoice_id,
                "user_id": mirror.user_id,
                "amount": mirror.price,
                "currency": mirror.currency,
                "retry": retry,
            }),
        );
    }

    /// Advance a subscription when its renewal invoice was just paid
    /// in full. Only the invoice recorded as the current renewal
    /// invoice renews: the initial order invoice and proration
    /// invoices never advance the period. The new period starts at
    /// the old period end, so a late payment never shortens the
    /// next period.
    fn maybe_renew_subscription(&self, invoice: &Invoice) {
        let Some(subscription_id) = &invoice.subscription_id else {
            return;
        };
        let Some(mirror) = self
            .subscription_lifecycles
            .get(subscription_id)
            .map(|entry| entry.value().clone())
        else {
            return;
        };
        if mirror.renewal_invoice_id.as_deref() != Some(invoice.id.as_str()) {
            return;
        }
        if invoice.status != "paid" || invoice.balance > 0.0 {
            return;
        }
        let start = mirror.current_period_end;
        let end = start + Duration::days(Self::period_days(&mirror.billing_period));
        self.emit(
            "subscription.lifecycle.apply",
            json!({
                "subscription_id": mirror.subscription_id,
                "action": "renew",
                "current_period_start": start.to_rfc3339(),
                "current_period_end": end.to_rfc3339(),
                "invoice_id": invoice.id,
            }),
        );
    }

    /// Ask the pricing module to reconcile an invoice before it is
    /// issued. Returns `Ok(())` only when every component matches
    /// (lines + discounts + proration + tax == total). A missing
    /// answer, an invalid answer, or a mismatch blocks issuing.
    /// Billing amounts are float major units (pre-existing); each
    /// one is converted through its 4-decimal string form so the
    /// pricing module parses it with integer math — this can only
    /// prove the rounded figures, not the unrounded float. Full
    /// proof needs billing on integer money. The invoice carries
    /// no separate discount or proration records (order coupons
    /// are allocated across the lines in `invoice_from_order_value`
    /// and proration credits travel as negative lines), so the
    /// `discounts` and `proration_lines` arrays are empty here by
    /// construction, not by omission. A caller-supplied total that
    /// differs from the computed one fails reconciliation by
    /// design: that is the guard working.
    fn validate_invoice_with_pricing(
        &self,
        tenant_id: &str,
        invoice: &Invoice,
    ) -> UnifiedResult<()> {
        let line = |item: &InvoiceItem| {
            json!({
                "name": item.name,
                "quantity": item.quantity,
                // 4-decimal string form of the float amount; the
                // pricing module parses it as scaled integer units.
                "unit_price": format!("{:.4}", item.unit_price),
                "tax_bp": (item.tax_rate * 10_000.0).round() as i64,
                "tax_inclusive": invoice.tax_inclusive,
            })
        };
        let context = self.bus().emit(
            "pricing.invoice.validate_requested",
            json!({
                "tenant_id": tenant_id,
                "invoice_id": invoice.id,
                "lines": invoice.items.iter().map(line).collect::<Vec<_>>(),
                "discounts": [],
                "proration_lines": [],
                "claimed": {
                    "subtotal": format!("{:.4}", invoice.subtotal),
                    "discounts": "0.0000",
                    "proration": "0.0000",
                    "tax_total": format!("{:.4}", invoice.tax_total),
                    "total": format!("{:.4}", invoice.total),
                },
                "currency": invoice.currency,
            }),
        );
        let answer = context.results.into_iter().next().ok_or_else(|| {
            UnifiedError::unprocessable(
                "Invoice reconciliation unavailable: no pricing validator answered",
            )
        })?;
        match answer.get("valid").and_then(|value| value.as_bool()) {
            Some(true) => Ok(()),
            _ => {
                let discrepancy = answer
                    .get("total_discrepancy")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
                    .or_else(|| {
                        answer
                            .get("total_discrepancy_scaled")
                            .and_then(|value| value.as_i64())
                            .map(|scaled| scaled.to_string())
                    })
                    .or_else(|| {
                        answer
                            .get("error")
                            .and_then(|value| value.as_str())
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| "unknown".to_string());
                Err(UnifiedError::unprocessable(format!(
                    "Invoice reconciliation failed (discrepancy {discrepancy}): issuing is blocked until the invoice figures match"
                )))
            }
        }
    }

    /// Build a renewal invoice for a subscription mirror and run
    /// it through pricing reconciliation for issue. On success the
    /// invoice is returned `issued`, exactly as before. On any
    /// validation failure — no pricing answer, a mismatch, or no
    /// tenant id on the mirror to validate against — the invoice
    /// is returned as a `draft` with the reason in its notes, so
    /// the caller must not request a charge for it; the next scan
    /// (or a manual issue, which validates with the request's
    /// tenant context) retries it. The tenant id here is the
    /// mirror's company id, which is the tenant id in the
    /// single-company deployment shape; when it is absent the
    /// invoice cannot be validated and is held as a draft.
    fn build_validated_renewal_invoice(
        &self,
        mirror: &SubscriptionLifecycle,
    ) -> (Invoice, UnifiedResult<()>) {
        let mut invoice = self.build_renewal_invoice(mirror);
        let validation = match &mirror.company_id {
            Some(tenant_id) if !tenant_id.trim().is_empty() => {
                self.validate_invoice_with_pricing(tenant_id.trim(), &invoice)
            }
            _ => Err(UnifiedError::unprocessable(
                "Invoice reconciliation unavailable: no tenant id on the subscription mirror",
            )),
        };
        match validation {
            Ok(()) => (invoice, Ok(())),
            Err(error) => {
                invoice.status = "draft".to_string();
                invoice.notes = Some(format!(
                    "Held as draft: invoice reconciliation did not pass ({}) — issue manually after review.",
                    error.message
                ));
                invoice.updated_at = now();
                (invoice, Err(error))
            }
        }
    }

    /// Validate a proration invoice for issue the same way as a
    /// renewal invoice: on failure the invoice is returned as a
    /// `draft` (with `paid_at` cleared when the credit covered the
    /// charge) instead of `issued` / `paid`, so no period or
    /// balance moves on an unreconciled invoice.
    fn validate_proration_invoice(
        &self,
        mirror: &SubscriptionLifecycle,
        invoice: Invoice,
    ) -> (Invoice, UnifiedResult<()>) {
        let validation = match &mirror.company_id {
            Some(tenant_id) if !tenant_id.trim().is_empty() => {
                self.validate_invoice_with_pricing(tenant_id.trim(), &invoice)
            }
            _ => Err(UnifiedError::unprocessable(
                "Invoice reconciliation unavailable: no tenant id on the subscription mirror",
            )),
        };
        match validation {
            Ok(()) => (invoice, Ok(())),
            Err(error) => {
                let mut held = invoice;
                held.status = "draft".to_string();
                held.paid_at = None;
                held.notes = Some(format!(
                    "Held as draft: invoice reconciliation did not pass ({}) — issue manually after review.",
                    error.message
                ));
                held.updated_at = now();
                (held, Err(error))
            }
        }
    }

    /// Run the subscription lifecycle scan over every mirrored
    /// subscription: trial ends, renewal reminders, renewal invoices
    /// with charge requests, dunning retries, past-due marking when
    /// a period ends unpaid, grace expiry, and period-end
    /// cancellations. Each decision is applied through
    /// `subscription.lifecycle.apply` so the store stays
    /// authoritative. The scan is driven by `cleanup.requested`
    /// (the scheduler's daily cleanup), by the
    /// `billing.subscriptions.scan` hook, and by
    /// `POST /api/v1/billing/subscriptions/scan`.
    fn run_subscription_scan(&self) -> Value {
        let at = now();
        let ids: Vec<String> = self
            .subscription_lifecycles
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        let mut trial_ended = 0u64;
        let mut reminders_sent = 0u64;
        let mut renewal_invoices = 0u64;
        let mut dunning_retries = 0u64;
        let mut marked_past_due = 0u64;
        let mut grace_cancellations = 0u64;
        let mut period_end_cancellations = 0u64;
        let mut details: Vec<Value> = Vec::new();
        for id in &ids {
            let Some(mirror) = self
                .subscription_lifecycles
                .get(id)
                .map(|entry| entry.value().clone())
            else {
                continue;
            };
            if mirror.status == "cancelled" {
                continue;
            }
            // Trial end: the trial is over, the subscription becomes
            // a paying one. The first paid period already runs to
            // trial end plus one billing period (set at creation).
            if mirror.status == "trialing" {
                if let Some(trial_end) = mirror.trial_end {
                    if trial_end <= at {
                        self.emit(
                            "subscription.lifecycle.apply",
                            json!({
                                "subscription_id": mirror.subscription_id,
                                "action": "end_trial",
                            }),
                        );
                        trial_ended += 1;
                        details.push(json!({
                            "subscription_id": mirror.subscription_id,
                            "action": "trial_ended",
                        }));
                        continue;
                    }
                }
            }
            // Cancellation scheduled for the end of the period.
            if mirror.cancel_at_period_end
                && matches!(mirror.status.as_str(), "active" | "trialing")
                && mirror.current_period_end <= at
            {
                self.emit(
                    "subscription.lifecycle.apply",
                    json!({
                        "subscription_id": mirror.subscription_id,
                        "action": "cancel",
                        "reason": "cancel_at_period_end",
                    }),
                );
                period_end_cancellations += 1;
                details.push(json!({
                    "subscription_id": mirror.subscription_id,
                    "action": "cancelled_at_period_end",
                }));
                continue;
            }
            // Past-due handling: grace expiry cancels, and while
            // grace remains a due retry is requested.
            if mirror.status == "past_due" {
                let grace_end = mirror.current_period_end
                    + Duration::days(i64::from(mirror.grace_period_days));
                if at > grace_end {
                    self.emit(
                        "subscription.lifecycle.apply",
                        json!({
                            "subscription_id": mirror.subscription_id,
                            "action": "cancel",
                            "reason": "grace_period_expired",
                        }),
                    );
                    grace_cancellations += 1;
                    details.push(json!({
                        "subscription_id": mirror.subscription_id,
                        "action": "cancelled_grace_expired",
                    }));
                    continue;
                }
                if mirror.dunning_attempts < mirror.dunning_max_retries {
                    if let Some(next_retry) = mirror.next_retry_at {
                        if next_retry <= at {
                            if let Some(invoice_id) = &mirror.renewal_invoice_id {
                                self.request_renewal_charge(&mirror, invoice_id, true);
                                dunning_retries += 1;
                                details.push(json!({
                                    "subscription_id": mirror.subscription_id,
                                    "action": "dunning_retry_requested",
                                    "attempt": mirror.dunning_attempts + 1,
                                }));
                            }
                        }
                    }
                }
                continue;
            }
            if !matches!(mirror.status.as_str(), "active" | "trialing") {
                continue;
            }
            // The period ended without a successful renewal payment:
            // the subscription goes past-due (grace and dunning
            // apply from here), instead of silently staying active.
            if mirror.current_period_end <= at {
                self.emit(
                    "subscription.lifecycle.apply",
                    json!({
                        "subscription_id": mirror.subscription_id,
                        "action": "mark_past_due",
                        "reason": "renewal_payment_not_received",
                    }),
                );
                marked_past_due += 1;
                details.push(json!({
                    "subscription_id": mirror.subscription_id,
                    "action": "marked_past_due",
                    "reason": "renewal_payment_not_received",
                }));
                continue;
            }
            let days_until_end = (mirror.current_period_end - at).num_days();
            // Renewal reminder, once per period.
            if days_until_end <= RENEWAL_REMINDER_LEAD_DAYS
                && mirror.renewal_reminder_sent_for != Some(mirror.current_period_end)
            {
                if let Some(mut stored) = self.subscription_lifecycles.get_mut(id) {
                    stored.renewal_reminder_sent_for = Some(mirror.current_period_end);
                    stored.updated_at = at;
                }
                self.request_notification(
                    &mirror.user_id,
                    "Your subscription renews soon",
                    &format!(
                        "Your subscription (product {}) renews on {} for {} {}.",
                        mirror.product_id,
                        mirror.current_period_end.to_rfc3339(),
                        mirror.price,
                        mirror.currency
                    ),
                    "subscription.renewal_reminder",
                );
                self.emit(
                    "subscription.renewal_reminder",
                    json!({
                        "subscription_id": mirror.subscription_id,
                        "user_id": mirror.user_id,
                        "current_period_end": mirror.current_period_end,
                        "price": mirror.price,
                        "currency": mirror.currency,
                    }),
                );
                reminders_sent += 1;
            }
            // Renewal invoice, once per period, before the period
            // ends — plus the charge request for it. The invoice
            // is reconciled before it is issued: when validation
            // fails it is stored as a draft instead (and the
            // period-end marker is left unset so the next scan
            // retries it), and no charge is requested for it.
            if !mirror.cancel_at_period_end
                && days_until_end <= RENEWAL_INVOICE_LEAD_DAYS
                && mirror.renewal_invoice_for_period_end != Some(mirror.current_period_end)
            {
                let (invoice, validation) = self.build_validated_renewal_invoice(&mirror);
                let invoice_id = invoice.id.clone();
                let issued = validation.is_ok();
                self.invoices.insert(invoice.id.clone(), invoice.clone());
                if issued {
                    if let Some(mut stored) = self.subscription_lifecycles.get_mut(id) {
                        stored.renewal_invoice_id = Some(invoice_id.clone());
                        stored.renewal_invoice_for_period_end =
                            Some(mirror.current_period_end);
                        stored.updated_at = at;
                    }
                }
                self.emit(
                    "invoice.created",
                    serde_json::to_value(&invoice).unwrap_or(Value::Null),
                );
                if issued {
                    self.request_renewal_charge(&mirror, &invoice_id, false);
                    renewal_invoices += 1;
                    details.push(json!({
                        "subscription_id": mirror.subscription_id,
                        "action": "renewal_invoice_created",
                        "invoice_id": invoice_id,
                    }));
                } else {
                    details.push(json!({
                        "subscription_id": mirror.subscription_id,
                        "action": "renewal_invoice_held_draft",
                        "invoice_id": invoice_id,
                        "reason": validation.err().map(|error| error.message),
                    }));
                }
            }
        }
        json!({
            "subscriptions_checked": ids.len(),
            "trial_ended": trial_ended,
            "reminders_sent": reminders_sent,
            "renewal_invoices_created": renewal_invoices,
            "dunning_retries_requested": dunning_retries,
            "marked_past_due": marked_past_due,
            "grace_cancellations": grace_cancellations,
            "period_end_cancellations": period_end_cancellations,
            "details": details,
        })
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

// ---------------------------------------------------------------------------
// Batch 18: payment reference ids (BEGIN — append-only section)
// ---------------------------------------------------------------------------
//
// Every invoice issued by this module carries one public payment
// reference in the single shared format (`PREFIX-12hex-12hex`, 30
// characters; see `crate::core::reference_id`). The sequential
// `number` (`INV-000001`) stays the human invoice number and is
// untouched: the reference is the gateway-facing payment id, the
// number is the document id. An invoice built from an order reuses
// that order's reference so the gateway sees one reference end to
// end; renewal, proration, manual, and fee invoices get a fresh
// reference at creation. References are issued once and never
// changed. The default prefix and per-product map are admin-editable
// settings held on `BillingState` (durable form: the
// `reference_id_settings` table in the migration snippet in the
// batch report).

/// Capability needed to read the reference settings.
const PERM_REFERENCE_READ: &str = "billing.reference:read";

/// Capability needed to change the reference settings.
const PERM_REFERENCE_MANAGE: &str = "billing.reference:manage";

/// Snapshot of the current reference settings.
fn reference_settings(state: &BillingState) -> ReferenceIdSettings {
    state
        .reference_settings
        .read()
        .expect("reference settings lock poisoned")
        .clone()
}

/// A fresh reference for an invoice with no source order, issued
/// with the configured default prefix.
fn fresh_invoice_reference(state: &BillingState) -> String {
    reference_id::generate_with_prefix(&reference_settings(state).default_prefix)
}

/// The reference for an invoice built from an order payload: the
/// order's own reference when the payload carries a valid one,
/// otherwise a fresh reference (orders issued before references
/// existed carry none).
fn invoice_reference_from_order(state: &BillingState, order: &Value) -> String {
    match order
        .get("reference")
        .and_then(|value| value.as_str())
        .filter(|reference| reference_id::is_valid(reference))
    {
        Some(reference) => reference.to_string(),
        None => fresh_invoice_reference(state),
    }
}

/// The reference for a manually created invoice: the seen order's
/// reference when an order id is given and this module has seen that
/// order's payload, otherwise a fresh reference.
fn invoice_reference_for_create(
    state: &BillingState,
    order_id: Option<&str>,
) -> String {
    match order_id.and_then(|id| state.seen_orders.get(id)) {
        Some(order) => invoice_reference_from_order(state, order.value()),
        None => fresh_invoice_reference(state),
    }
}

/// Admin input for `PUT /api/v1/billing/reference-settings`. All
/// fields are optional and applied together: `default_prefix`
/// replaces the default prefix, `product_prefixes` replaces the
/// whole per-product map, and `remove_product_prefixes` deletes
/// individual overrides. Every prefix is normalized with
/// `format_prefix` before it is stored.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateReferenceSettings {
    pub default_prefix: Option<String>,
    pub product_prefixes: Option<HashMap<String, String>>,
    pub remove_product_prefixes: Option<Vec<String>>,
}

/// Read the reference settings (capability `billing.reference:read`).
async fn get_reference_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
) -> HttpResponse {
    let result: UnifiedResult<ReferenceIdSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_REFERENCE_READ)?;
        Ok(reference_settings(&state))
    })();
    json::respond(result, false)
}

/// Update the reference settings (capability
/// `billing.reference:manage`). Already-issued references keep
/// validating: changing a prefix never rewrites an issued id.
async fn update_reference_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    body: web::Json<UpdateReferenceSettings>,
) -> HttpResponse {
    let result: UnifiedResult<ReferenceIdSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_REFERENCE_MANAGE)?;
        let input = body.into_inner();
        let mut candidate = reference_settings(&state);
        if let Some(prefix) = input.default_prefix {
            candidate.default_prefix = reference_id::format_prefix(&prefix);
        }
        if let Some(prefixes) = input.product_prefixes {
            let mut next = ReferenceIdSettings::default();
            next.default_prefix = candidate.default_prefix.clone();
            for (key, prefix) in prefixes {
                next.set_product_prefix(&key, &prefix).map_err(|problem| {
                    UnifiedError::bad_request(problem)
                })?;
            }
            candidate = next;
        }
        if let Some(keys) = input.remove_product_prefixes {
            for key in keys {
                candidate.remove_product_prefix(&key);
            }
        }
        let problems = candidate.validate();
        if !problems.is_empty() {
            return Err(UnifiedError::unprocessable(format!(
                "Reference settings are invalid: {}",
                problems.join("; ")
            )));
        }
        {
            let mut settings = state
                .reference_settings
                .write()
                .expect("reference settings lock poisoned");
            *settings = candidate.clone();
        }
        state.emit(
            reference_id::EVENT_REFERENCE_SETTINGS_CHANGED,
            serde_json::to_value(&candidate).unwrap_or(Value::Null),
        );
        Ok(candidate)
    })();
    json::respond(result, false)
}

// Batch 18: payment reference ids (END)
// ---------------------------------------------------------------------------

fn round_money(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// True for zero-decimal currencies (no minor units), matching the
/// payments module's list.
fn is_zero_decimal_currency(currency: &str) -> bool {
    matches!(
        currency,
        "JPY" | "KRW" | "VND" | "CLP" | "ISK" | "UGX" | "XAF" | "XOF" | "XPF"
    )
}

/// Convert integer minor units (as the payments module reports
/// amounts) to major units (as invoices store them).
fn minor_to_major(amount_minor: i64, currency: &str) -> f64 {
    if is_zero_decimal_currency(currency) {
        amount_minor as f64
    } else {
        round_money(amount_minor as f64 / 100.0)
    }
}

/// Caller identities from the request headers (`x-user-id` and
/// `x-session-id`). See the store module: this is an ownership check
/// on self-asserted header identity, not authentication. A caller
/// with no identity header matches only `guest`-owned records.
fn caller_identities(req: &HttpRequest) -> Vec<String> {
    ["x-user-id", "x-session-id"]
        .into_iter()
        .filter_map(|header| {
            req.headers()
                .get(header)
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .collect()
}

/// True when the caller may act on a record owned by `owner`.
fn caller_owns(req: &HttpRequest, owner: &str) -> bool {
    let identities = caller_identities(req);
    if identities.is_empty() {
        return owner == "guest";
    }
    identities.iter().any(|identity| identity == owner)
}

/// Reject a mutating invoice call from a caller who does not own the
/// invoice (see `caller_owns`).
fn require_invoice_owner(req: &HttpRequest, invoice: &Invoice) -> UnifiedResult<()> {
    if caller_owns(req, &invoice.user_id) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Invoice belongs to a different user",
        ))
    }
}

/// Split one line amount into (net amount, tax amount) for a tax rate
/// given as a fraction (0.10 means 10%). When tax is inclusive the
/// amount already contains the tax; otherwise the tax is added on top.
fn line_tax_on_amount(amount: f64, tax_rate: f64, tax_inclusive: bool) -> (f64, f64) {
    let amount = round_money(amount);
    if tax_inclusive {
        let net = round_money(amount / (1.0 + tax_rate));
        (net, round_money(amount - net))
    } else {
        (amount, round_money(amount * tax_rate))
    }
}

/// Split `unit_price * quantity` into (net amount, tax amount).
fn line_tax(unit_price: f64, quantity: u32, tax_rate: f64, tax_inclusive: bool) -> (f64, f64) {
    line_tax_on_amount(unit_price * quantity as f64, tax_rate, tax_inclusive)
}

/// True when an invoice is past its due date with a balance still
/// outstanding. Paid, void and credited invoices are never overdue.
fn is_overdue(invoice: &Invoice, at: DateTime<Utc>) -> bool {
    matches!(
        invoice.status.as_str(),
        "draft" | "issued" | "partially_paid" | "unpaid"
    ) && invoice.balance > 0.0
        && invoice.due_date.is_some_and(|due| due < at)
}

/// The status callers see: an unpaid invoice past its due date reads
/// as `overdue`. The stored status is left untouched.
fn effective_status(invoice: &Invoice, at: DateTime<Utc>) -> String {
    if is_overdue(invoice, at) {
        "overdue".to_string()
    } else {
        invoice.status.clone()
    }
}

/// A copy of an invoice with the derived `overdue` flag and effective
/// status filled in. All read endpoints return this view.
fn view_invoice(invoice: &Invoice) -> Invoice {
    let mut view = invoice.clone();
    view.overdue = is_overdue(invoice, now());
    if view.overdue {
        view.status = "overdue".to_string();
    }
    view
}

/// Recompute the outstanding balance from the totals. The balance is
/// never negative: overpayment and over-crediting are rejected before
/// they can happen.
fn recompute_balance(invoice: &mut Invoice) {
    invoice.balance = round_money(
        (invoice.total - invoice.amount_paid - invoice.credit_total).max(0.0),
    );
}

/// Reminder offsets that fall due today for one invoice, in days
/// relative to the due date (positive = days before due, negative =
/// days after due). Offsets already sent are excluded.
fn reminders_due(invoice: &Invoice, settings: &BillingSettings, at: DateTime<Utc>) -> Vec<i32> {
    let Some(due) = invoice.due_date else {
        return Vec::new();
    };
    if invoice.balance <= 0.0
        || matches!(invoice.status.as_str(), "paid" | "void" | "credited")
    {
        return Vec::new();
    }
    let today = at.date_naive();
    let due_day = due.date_naive();
    let mut due_offsets: Vec<i32> = Vec::new();
    for &days in &settings.reminder_before_days {
        if days > 0 && today == due_day - Duration::days(i64::from(days)) {
            due_offsets.push(days);
        }
    }
    for &days in &settings.reminder_after_days {
        if days > 0 && today == due_day + Duration::days(i64::from(days)) {
            due_offsets.push(-days);
        }
    }
    due_offsets.retain(|offset| !invoice.reminders_sent.contains(offset));
    due_offsets
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

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn render_invoice_html(invoice: &Invoice) -> String {
    let mut rows = String::new();
    for item in &invoice.items {
        rows.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{:.2}</td><td>{:.2}</td><td>{:.2}</td></tr>",
            escape_html(&item.name),
            item.quantity,
            item.unit_price,
            item.tax_amount,
            item.line_total
        ));
    }
    let due = invoice
        .due_date
        .map(|due| due.to_rfc3339())
        .unwrap_or_else(|| "-".to_string());
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Invoice {}</title></head>\
        <body><h1>Invoice {}</h1>\
        <p>Status: {}<br>User: {}<br>Currency: {}<br>Due date: {}<br>\
        Subtotal: {:.2}<br>Tax: {:.2}<br>Total: {:.2}<br>Paid: {:.2}<br>Balance: {:.2}</p>\
        <table><thead><tr><th>Item</th><th>Quantity</th><th>Unit price</th><th>Tax</th><th>Total</th></tr></thead>\
        <tbody>{}</tbody></table></body></html>",
        escape_html(&invoice.number),
        escape_html(&invoice.number),
        escape_html(&invoice.status),
        escape_html(&invoice.user_id),
        escape_html(&invoice.currency),
        escape_html(&due),
        invoice.subtotal,
        invoice.tax_total,
        invoice.total,
        invoice.amount_paid,
        invoice.balance,
        rows
    )
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_invoices(
    state: web::Data<Arc<BillingState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let sort = params.allowed_sort(&["created_at", "total", "due_date"], "created_at");
    let mut items: Vec<Invoice> = state
        .invoices
        .iter()
        .map(|entry| view_invoice(entry.value()))
        .filter(|invoice| {
            matches_search(&format!("{} {} {}", invoice.number, invoice.id, invoice.user_id), &params)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| invoice.status == *status)
                && params
                    .filters
                    .get("user_id")
                    .is_none_or(|user_id| invoice.user_id == *user_id)
                && params.filters.get("order_id").is_none_or(|order_id| {
                    invoice.order_id.as_deref() == Some(order_id.as_str())
                })
                && params.filters.get("company_id").is_none_or(|company_id| {
                    invoice.company_id.as_deref() == Some(company_id.as_str())
                })
        })
        .collect();
    match sort.as_str() {
        "total" => items.sort_by(|a, b| {
            a.total
                .partial_cmp(&b.total)
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        "due_date" => items.sort_by(|a, b| a.due_date.cmp(&b.due_date)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_invoice(
    state: web::Data<Arc<BillingState>>,
    body: web::Json<CreateInvoice>,
) -> HttpResponse {
    let result: UnifiedResult<Invoice> = (|| {
        let input = body.into_inner();
        if input.user_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("user_id must not be empty"));
        }
        if input.items.is_empty() {
            return Err(UnifiedError::bad_request(
                "An invoice needs at least one item",
            ));
        }
        let settings = state.settings();
        let tax_inclusive = input.tax_inclusive.unwrap_or(settings.tax_inclusive);
        let payment_terms_days = input
            .payment_terms_days
            .unwrap_or(settings.payment_terms_days);
        let mut items: Vec<InvoiceItem> = Vec::new();
        let mut subtotal = 0.0;
        let mut tax_total = 0.0;
        for item in input.items {
            if item.name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Item name must not be empty"));
            }
            if !item.unit_price.is_finite() || item.unit_price < 0.0 {
                return Err(UnifiedError::bad_request(
                    "Item unit price must be a non-negative number",
                ));
            }
            let quantity = item.quantity.unwrap_or(1);
            if quantity == 0 {
                return Err(UnifiedError::bad_request("Item quantity must be at least 1"));
            }
            let tax_rate = item.tax_rate.unwrap_or(settings.default_tax_rate);
            if !tax_rate.is_finite() || !(0.0..=1.0).contains(&tax_rate) {
                return Err(UnifiedError::bad_request(
                    "Item tax rate must be a fraction between 0 and 1",
                ));
            }
            let (net, tax) = line_tax(item.unit_price, quantity, tax_rate, tax_inclusive);
            let line_total = if tax_inclusive {
                round_money(net + tax)
            } else {
                net
            };
            subtotal += net;
            tax_total += tax;
            items.push(InvoiceItem {
                name: item.name.trim().to_string(),
                quantity,
                unit_price: round_money(item.unit_price),
                line_total,
                tax_rate,
                tax_amount: tax,
            });
        }
        let computed_total = round_money(subtotal + tax_total);
        let total = round_money(input.total.unwrap_or(computed_total));
        if total < 0.0 || !total.is_finite() {
            return Err(UnifiedError::bad_request("Invoice total must be non-negative"));
        }
        // An explicit total overrides the computed one, as before:
        // it is the amount due, while subtotal and tax_total still
        // describe the line breakdown. Callers that omit the total
        // get the computed subtotal + tax.
        let currency = input.currency.unwrap_or_else(|| "USD".to_string());
        if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(UnifiedError::bad_request(
                "Currency must be a 3-letter code such as USD",
            ));
        }
        let created = now();
        let invoice_reference =
            invoice_reference_for_create(&**state, input.order_id.as_deref());
        let invoice = Invoice {
            id: new_id(),
            number: state.next_invoice_number(),
            reference: invoice_reference,
            order_id: input.order_id,
            subscription_id: None,
            user_id: input.user_id.trim().to_string(),
            company_id: input.company_id,
            items,
            subtotal: round_money(subtotal),
            tax_total: round_money(tax_total),
            total,
            currency: currency.to_uppercase(),
            status: "draft".to_string(),
            overdue: false,
            tax_inclusive,
            amount_paid: 0.0,
            credit_total: 0.0,
            balance: total,
            issue_date: Some(created),
            due_date: Some(
                input
                    .due_date
                    .unwrap_or(created + Duration::days(i64::from(payment_terms_days))),
            ),
            payment_terms_days: Some(payment_terms_days),
            paid_at: None,
            notes: input.notes.filter(|notes| !notes.trim().is_empty()),
            reminders_sent: Vec::new(),
            created_at: created,
            updated_at: created,
        };
        state.invoices.insert(invoice.id.clone(), invoice.clone());
        state.emit(
            "invoice.created",
            serde_json::to_value(&invoice).unwrap_or(Value::Null),
        );
        Ok(view_invoice(&invoice))
    })();
    respond_created(result)
}

async fn from_order(
    state: web::Data<Arc<BillingState>>,
    body: web::Json<FromOrderRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Invoice> = (|| {
        let input = body.into_inner();
        // Reuse an existing invoice for this order when one already exists
        // (for example the draft created automatically by the hook).
        if let Some(existing) = state
            .invoices
            .iter()
            .find(|entry| entry.value().order_id.as_deref() == Some(input.order_id.as_str()))
            .map(|entry| view_invoice(entry.value()))
        {
            return Ok(existing);
        }
        let order = state
            .seen_orders
            .get(&input.order_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(
                    "Order not found: billing has not seen this order yet. \
                    Place the order through the store first, or create the \
                    invoice directly with items.",
                )
            })?;
        // A caller-supplied user id must match the order's owner:
        // otherwise one user could re-issue another user's order
        // under their own name.
        if let Some(user_id) = input.user_id.as_deref().filter(|id| !id.trim().is_empty()) {
            let order_user = order
                .get("user_id")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if user_id.trim() != order_user {
                return Err(UnifiedError::unprocessable(
                    "user_id does not match the owner of this order",
                ));
            }
        }
        let mut invoice = state
            .invoice_from_order_value(&order)
            .ok_or_else(|| UnifiedError::unprocessable("Stored order data is incomplete"))?;
        if let Some(user_id) = input.user_id.filter(|id| !id.trim().is_empty()) {
            invoice.user_id = user_id;
        }
        if let Some(due_date) = input.due_date {
            invoice.due_date = Some(due_date);
        }
        state.invoices.insert(invoice.id.clone(), invoice.clone());
        state.emit(
            "invoice.created",
            serde_json::to_value(&invoice).unwrap_or(Value::Null),
        );
        Ok(view_invoice(&invoice))
    })();
    respond_created(result)
}

async fn get_invoice(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Invoice> = (|| {
        let invoice = state
            .invoices
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        require_invoice_owner(&req, &invoice)?;
        Ok(view_invoice(&invoice))
    })();
    json::respond(result, false)
}

async fn update_invoice(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateInvoice>,
) -> HttpResponse {
    let result: UnifiedResult<Invoice> = (|| {
        let input = body.into_inner();
        let mut invoice = state
            .invoices
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        require_invoice_owner(&req, &invoice)?;
        if matches!(
            invoice.status.as_str(),
            "paid" | "void" | "partially_paid" | "credited"
        ) {
            return Err(UnifiedError::unprocessable(format!(
                "A {} invoice can no longer be edited",
                invoice.status
            )));
        }
        if let Some(status) = input.status {
            if !matches!(status.as_str(), "draft" | "issued") {
                return Err(UnifiedError::unprocessable(
                    "Status can only be changed to draft or issued here; use pay or void for the rest",
                ));
            }
            // Issuing is the enforcement point for pricing
            // reconciliation: a draft becomes `issued` only when
            // the pricing module reconciles every component. The
            // tenant id comes from the request's resolved tenant
            // context, never from a client header; without one,
            // issuing is blocked (fail closed).
            if status == "issued" && invoice.status != "issued" {
                let tenant = crate::core::tenancy::TenantContext::from_http_request(&req)
                    .map_err(|_| {
                        UnifiedError::unprocessable(
                            "Invoice reconciliation requires a resolved tenant context; issuing is blocked",
                        )
                    })?;
                state.validate_invoice_with_pricing(&tenant.tenant_id.to_string(), &invoice)?;
            }
            invoice.status = status;
        }
        if let Some(due_date) = input.due_date {
            invoice.due_date = Some(due_date);
        }
        if let Some(notes) = input.notes {
            invoice.notes = if notes.trim().is_empty() {
                None
            } else {
                Some(notes)
            };
        }
        invoice.updated_at = now();
        Ok(view_invoice(&invoice))
    })();
    json::respond(result, false)
}

async fn delete_invoice(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let invoice = state
            .invoices
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        require_invoice_owner(&req, &invoice)?;
        if invoice.status != "draft" {
            return Err(UnifiedError::unprocessable(
                "Only draft invoices can be deleted; void the invoice instead",
            ));
        }
        state.invoices.remove(path.as_str());
        // A draft has no payments or credit notes in normal use, but
        // remove any that exist so no orphan records remain.
        let payment_ids: Vec<String> = state
            .payments
            .iter()
            .filter(|entry| entry.value().invoice_id == path.as_str())
            .map(|entry| entry.key().clone())
            .collect();
        for id in payment_ids {
            state.payments.remove(&id);
        }
        let credit_ids: Vec<String> = state
            .credit_notes
            .iter()
            .filter(|entry| entry.value().invoice_id == path.as_str())
            .map(|entry| entry.key().clone())
            .collect();
        for id in credit_ids {
            state.credit_notes.remove(&id);
        }
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, false)
}

async fn pay_invoice(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PayInvoiceRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Invoice> = (|| {
        let method = body
            .into_inner()
            .method
            .unwrap_or_else(|| "manual".to_string());
        if method.trim().is_empty() {
            return Err(UnifiedError::bad_request("Payment method must not be empty"));
        }
        let mut invoice = state
            .invoices
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        require_invoice_owner(&req, &invoice)?;
        if invoice.status == "paid" {
            return Err(UnifiedError::conflict("Invoice is already paid"));
        }
        if invoice.status == "void" {
            return Err(UnifiedError::unprocessable("A void invoice cannot be paid"));
        }
        if invoice.status == "credited" {
            return Err(UnifiedError::unprocessable(
                "A credited invoice cannot be paid",
            ));
        }
        let remaining = invoice.balance;
        if remaining <= 0.0 {
            return Err(UnifiedError::conflict(
                "Invoice has no outstanding balance",
            ));
        }
        // Full payment: record it as a payment entry so the payment
        // history is complete, then close the invoice.
        let payment = InvoicePayment {
            id: new_id(),
            invoice_id: invoice.id.clone(),
            amount: remaining,
            method: method.clone(),
            reference: None,
            recorded_at: now(),
        };
        state.payments.insert(payment.id.clone(), payment.clone());
        invoice.amount_paid = round_money(invoice.amount_paid + remaining);
        recompute_balance(&mut invoice);
        invoice.status = "paid".to_string();
        invoice.paid_at = Some(now());
        invoice.updated_at = now();
        let invoice_value = serde_json::to_value(&*invoice).unwrap_or(Value::Null);
        let payment_value = serde_json::to_value(&payment).unwrap_or(Value::Null);
        drop(invoice);
        state.emit("invoice.payment.recorded", payment_value);
        state.emit(
            "invoice.paid",
            json!({ "invoice": invoice_value, "method": method }),
        );
        let paid = state
            .invoices
            .get(path.as_str())
            .map(|entry| view_invoice(entry.value()))
            .expect("invoice still present");
        // A manually paid renewal invoice advances the subscription
        // period exactly like a captured gateway payment.
        state.maybe_renew_subscription(&paid);
        Ok(paid)
    })();
    json::respond(result, false)
}

async fn void_invoice(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Invoice> = (|| {
        let mut invoice = state
            .invoices
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        require_invoice_owner(&req, &invoice)?;
        if invoice.status == "paid" {
            return Err(UnifiedError::unprocessable("A paid invoice cannot be voided"));
        }
        if invoice.status == "void" {
            return Err(UnifiedError::conflict("Invoice is already void"));
        }
        invoice.status = "void".to_string();
        invoice.updated_at = now();
        let invoice_value = serde_json::to_value(&*invoice).unwrap_or(Value::Null);
        drop(invoice);
        state.emit("invoice.voided", invoice_value);
        Ok(state
            .invoices
            .get(path.as_str())
            .map(|entry| view_invoice(entry.value()))
            .expect("invoice still present"))
    })();
    json::respond(result, false)
}

async fn render_invoice(
    state: web::Data<Arc<BillingState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let invoice = state
            .invoices
            .get(path.as_str())
            .map(|entry| view_invoice(entry.value()))
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        let html = render_invoice_html(&invoice);
        Ok(json!({ "invoice": invoice, "html": html }))
    })();
    json::respond(result, false)
}

async fn list_overdue(
    state: web::Data<Arc<BillingState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<Invoice> = state
        .invoices
        .iter()
        .map(|entry| view_invoice(entry.value()))
        .filter(|invoice| {
            invoice.overdue
                && matches_search(&format!("{} {} {}", invoice.number, invoice.id, invoice.user_id), &params)
                && params
                    .filters
                    .get("user_id")
                    .is_none_or(|user_id| invoice.user_id == *user_id)
        })
        .collect();
    items.sort_by(|a, b| a.due_date.cmp(&b.due_date));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn get_settings(state: web::Data<Arc<BillingState>>) -> HttpResponse {
    let settings = state.settings();
    json::success_response(
        json!({
            "invoice_prefix": settings.invoice_prefix,
            "default_tax_rate": settings.default_tax_rate,
            "tax_inclusive": settings.tax_inclusive,
            "payment_terms_days": settings.payment_terms_days,
            "reminder_before_days": settings.reminder_before_days,
            "reminder_after_days": settings.reminder_after_days,
            "next_invoice_number": state.next_invoice_number_preview(),
        }),
        200,
    )
}

async fn update_settings(
    state: web::Data<Arc<BillingState>>,
    body: web::Json<UpdateBillingSettings>,
) -> HttpResponse {
    let result: UnifiedResult<BillingSettings> = (|| {
        let input = body.into_inner();
        if let Some(prefix) = &input.invoice_prefix {
            let prefix = prefix.trim();
            if prefix.is_empty()
                || prefix.len() > 16
                || !prefix
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err(UnifiedError::bad_request(
                    "invoice_prefix must be 1-16 letters, digits, '-' or '_'",
                ));
            }
        }
        if let Some(rate) = input.default_tax_rate {
            if !rate.is_finite() || !(0.0..=1.0).contains(&rate) {
                return Err(UnifiedError::bad_request(
                    "default_tax_rate must be a fraction between 0 and 1",
                ));
            }
        }
        if let Some(days) = input.payment_terms_days {
            if days > 365 {
                return Err(UnifiedError::bad_request(
                    "payment_terms_days must be between 0 and 365",
                ));
            }
        }
        for (name, days) in [
            ("reminder_before_days", &input.reminder_before_days),
            ("reminder_after_days", &input.reminder_after_days),
        ] {
            if let Some(days) = days {
                if days.len() > 30 || days.iter().any(|day| *day < 0 || *day > 365) {
                    return Err(UnifiedError::bad_request(format!(
                        "{name} must hold at most 30 entries, each between 0 and 365"
                    )));
                }
            }
        }
        let mut settings = state
            .settings
            .write()
            .expect("billing settings lock poisoned");
        if let Some(prefix) = input.invoice_prefix {
            settings.invoice_prefix = prefix.trim().to_string();
        }
        if let Some(rate) = input.default_tax_rate {
            settings.default_tax_rate = rate;
        }
        if let Some(inclusive) = input.tax_inclusive {
            settings.tax_inclusive = inclusive;
        }
        if let Some(days) = input.payment_terms_days {
            settings.payment_terms_days = days;
        }
        if let Some(days) = input.reminder_before_days {
            settings.reminder_before_days = days;
        }
        if let Some(days) = input.reminder_after_days {
            settings.reminder_after_days = days;
        }
        Ok(settings.clone())
    })();
    json::respond(result, false)
}

async fn record_payment(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RecordPaymentRequest>,
) -> HttpResponse {
    let result: UnifiedResult<InvoicePayment> = (|| {
        let input = body.into_inner();
        if !input.amount.is_finite() || input.amount <= 0.0 {
            return Err(UnifiedError::bad_request(
                "Payment amount must be a positive number",
            ));
        }
        let method = input
            .method
            .unwrap_or_else(|| "manual".to_string());
        if method.trim().is_empty() {
            return Err(UnifiedError::bad_request("Payment method must not be empty"));
        }
        let reference = input
            .reference
            .filter(|reference| !reference.trim().is_empty())
            .map(|reference| {
                if reference.len() > 128 {
                    reference.chars().take(128).collect()
                } else {
                    reference
                }
            });
        let mut invoice = state
            .invoices
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        require_invoice_owner(&req, &invoice)?;
        if invoice.status == "paid" {
            return Err(UnifiedError::conflict("Invoice is already paid"));
        }
        if invoice.status == "void" {
            return Err(UnifiedError::unprocessable("A void invoice cannot be paid"));
        }
        if invoice.status == "credited" {
            return Err(UnifiedError::unprocessable(
                "A credited invoice cannot be paid",
            ));
        }
        let amount = round_money(input.amount);
        if amount > invoice.balance {
            return Err(UnifiedError::unprocessable(format!(
                "Payment of {:.2} exceeds the outstanding balance of {:.2}",
                amount, invoice.balance
            )));
        }
        let payment = InvoicePayment {
            id: new_id(),
            invoice_id: invoice.id.clone(),
            amount,
            method: method.clone(),
            reference,
            recorded_at: now(),
        };
        state.payments.insert(payment.id.clone(), payment.clone());
        invoice.amount_paid = round_money(invoice.amount_paid + amount);
        recompute_balance(&mut invoice);
        if invoice.balance == 0.0 {
            invoice.status = "paid".to_string();
            invoice.paid_at = Some(now());
        } else {
            invoice.status = "partially_paid".to_string();
        }
        invoice.updated_at = now();
        let fully_paid = invoice.status == "paid";
        let invoice_value = serde_json::to_value(&*invoice).unwrap_or(Value::Null);
        let payment_value = serde_json::to_value(&payment).unwrap_or(Value::Null);
        drop(invoice);
        state.emit("invoice.payment.recorded", payment_value);
        if fully_paid {
            state.emit(
                "invoice.paid",
                json!({ "invoice": invoice_value, "method": method }),
            );
        }
        Ok(payment)
    })();
    match result {
        Ok(payment) => json::success_response(payment, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn list_payments(
    state: web::Data<Arc<BillingState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    if !state.invoices.contains_key(path.as_str()) {
        return json::error_response(&UnifiedError::not_found("Invoice not found"), false);
    }
    let mut items: Vec<InvoicePayment> = state
        .payments
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|payment| {
            payment.invoice_id == path.as_str()
                && matches_search(
                    &format!("{} {}", payment.method, payment.reference.clone().unwrap_or_default()),
                    &params,
                )
        })
        .collect();
    items.sort_by(|a, b| a.recorded_at.cmp(&b.recorded_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_credit_note(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateCreditNoteRequest>,
) -> HttpResponse {
    let result: UnifiedResult<CreditNote> = (|| {
        let input = body.into_inner();
        if !input.amount.is_finite() || input.amount <= 0.0 {
            return Err(UnifiedError::bad_request(
                "Credit note amount must be a positive number",
            ));
        }
        if input.reason.trim().is_empty() {
            return Err(UnifiedError::bad_request("Credit note reason must not be empty"));
        }
        if input.reason.len() > 500 {
            return Err(UnifiedError::bad_request(
                "Credit note reason must be at most 500 characters",
            ));
        }
        let mut invoice = state
            .invoices
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        require_invoice_owner(&req, &invoice)?;
        if invoice.status == "void" {
            return Err(UnifiedError::unprocessable(
                "A void invoice cannot receive a credit note",
            ));
        }
        let amount = round_money(input.amount);
        if amount > invoice.balance {
            return Err(UnifiedError::unprocessable(format!(
                "Credit note of {:.2} exceeds the outstanding balance of {:.2}",
                amount, invoice.balance
            )));
        }
        let credit_note = CreditNote {
            id: new_id(),
            invoice_id: invoice.id.clone(),
            number: state.next_credit_note_number(),
            amount,
            reason: input.reason.trim().to_string(),
            created_at: now(),
        };
        state
            .credit_notes
            .insert(credit_note.id.clone(), credit_note.clone());
        invoice.credit_total = round_money(invoice.credit_total + amount);
        recompute_balance(&mut invoice);
        if invoice.balance == 0.0 {
            invoice.status = "credited".to_string();
        }
        invoice.updated_at = now();
        let credit_value = serde_json::to_value(&credit_note).unwrap_or(Value::Null);
        drop(invoice);
        state.emit("invoice.credit_note.created", credit_value);
        Ok(credit_note)
    })();
    match result {
        Ok(credit_note) => json::success_response(credit_note, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn list_credit_notes(
    state: web::Data<Arc<BillingState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    if !state.invoices.contains_key(path.as_str()) {
        return json::error_response(&UnifiedError::not_found("Invoice not found"), false);
    }
    let mut items: Vec<CreditNote> = state
        .credit_notes
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|note| {
            note.invoice_id == path.as_str()
                && matches_search(&format!("{} {}", note.number, note.reason), &params)
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn pdf_data(
    state: web::Data<Arc<BillingState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let invoice = state
            .invoices
            .get(path.as_str())
            .map(|entry| view_invoice(entry.value()))
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        let payments: Vec<InvoicePayment> = state
            .payments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|payment| payment.invoice_id == invoice.id)
            .collect();
        let credit_notes: Vec<CreditNote> = state
            .credit_notes
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|note| note.invoice_id == invoice.id)
            .collect();
        // PDF rendering itself is a follow-on step: feed this payload
        // to a PDF renderer (for example a headless-browser or
        // print-CSS pipeline). This endpoint returns data only, never
        // a rendered file.
        Ok(json!({
            "invoice": invoice,
            "company": {
                "id": invoice.company_id,
                "name": Value::Null,
                "note": "Populate company details from the company module or settings before rendering.",
            },
            "customer": { "user_id": invoice.user_id },
            "lines": invoice.items,
            "totals": {
                "subtotal": invoice.subtotal,
                "tax_total": invoice.tax_total,
                "total": invoice.total,
                "amount_paid": invoice.amount_paid,
                "credit_total": invoice.credit_total,
                "balance": invoice.balance,
                "currency": invoice.currency,
                "tax_inclusive": invoice.tax_inclusive,
            },
            "dates": {
                "issue_date": invoice.issue_date,
                "due_date": invoice.due_date,
                "paid_at": invoice.paid_at,
            },
            "payments": payments,
            "credit_notes": credit_notes,
            "notes": invoice.notes,
            "renderer": "PDF rendering is a follow-on step; this payload is shaped for a PDF renderer.",
        }))
    })();
    json::respond(result, false)
}

/// Emit one overdue reminder for an invoice. The reminder itself is
/// delivered by the mail module through the `billing.invoice.reminder`
/// hook; billing only decides that a reminder is due and records that
/// it was sent.
fn send_one_reminder(
    state: &BillingState,
    invoice_id: &str,
    offset_days: i32,
    message: Option<String>,
    manual: bool,
) -> UnifiedResult<Value> {
    let mut invoice = state
        .invoices
        .get_mut(invoice_id)
        .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
    if invoice.balance <= 0.0 {
        return Err(UnifiedError::unprocessable(
            "Invoice has no outstanding balance to remind about",
        ));
    }
    if matches!(invoice.status.as_str(), "paid" | "void" | "credited") {
        return Err(UnifiedError::unprocessable(format!(
            "A {} invoice cannot receive reminders",
            invoice.status
        )));
    }
    let due_date = invoice
        .due_date
        .ok_or_else(|| UnifiedError::unprocessable("Invoice has no due date"))?;
    if !manual && invoice.reminders_sent.contains(&offset_days) {
        return Err(UnifiedError::conflict(
            "This reminder was already sent for the invoice",
        ));
    }
    if !invoice.reminders_sent.contains(&offset_days) {
        invoice.reminders_sent.push(offset_days);
    }
    invoice.updated_at = now();
    let days_until_due = (due_date.date_naive() - now().date_naive()).num_days();
    let invoice_value = serde_json::to_value(view_invoice(&invoice)).unwrap_or(Value::Null);
    drop(invoice);
    state.emit(
        "billing.invoice.reminder",
        json!({
            "invoice": invoice_value,
            "invoice_id": invoice_id,
            "offset_days": offset_days,
            "days_until_due": days_until_due,
            "due_date": due_date,
            "manual": manual,
            "message": message,
            "note": "Deliver this reminder through the mail module; billing only emits the hook.",
        }),
    );
    Ok(json!({
        "invoice_id": invoice_id,
        "offset_days": offset_days,
        "days_until_due": days_until_due,
        "manual": manual,
        "sent": true,
    }))
}

async fn remind_invoice(
    state: web::Data<Arc<BillingState>>,
    path: web::Path<String>,
    body: web::Json<RemindRequest>,
) -> HttpResponse {
    let input = body.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let invoice = state
            .invoices
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        let offset_days = match input.offset_days {
            Some(offset) => offset,
            None => {
                let due = invoice
                    .due_date
                    .ok_or_else(|| UnifiedError::unprocessable("Invoice has no due date"))?;
                let days = (due.date_naive() - now().date_naive()).num_days();
                i32::try_from(days).unwrap_or(0)
            }
        };
        send_one_reminder(&state, path.as_str(), offset_days, input.message, true)
    })();
    json::respond(result, false)
}

/// Scan every open invoice for reminders that fall due today and emit
/// one `billing.invoice.reminder` hook per reminder. Returns a summary
/// of what was sent.
fn run_reminder_scan(state: &BillingState) -> Value {
    let settings = state.settings();
    let at = now();
    let invoice_ids: Vec<String> = state
        .invoices
        .iter()
        .map(|entry| entry.key().clone())
        .collect();
    let mut sent = 0u64;
    let mut details: Vec<Value> = Vec::new();
    for invoice_id in &invoice_ids {
        let Some(invoice) = state
            .invoices
            .get(invoice_id)
            .map(|entry| entry.value().clone())
        else {
            continue;
        };
        for offset in reminders_due(&invoice, &settings, at) {
            match send_one_reminder(state, invoice_id, offset, None, false) {
                Ok(detail) => {
                    sent += 1;
                    details.push(detail);
                }
                Err(_) => {
                    // A reminder that can no longer be sent (invoice
                    // paid or credited since the scan started) is
                    // skipped; the scan continues with the rest.
                }
            }
        }
    }
    json!({
        "reminders_sent": sent,
        "invoices_checked": invoice_ids.len(),
        "details": details,
    })
}

async fn run_reminders(state: web::Data<Arc<BillingState>>) -> HttpResponse {
    let summary = run_reminder_scan(&state);
    state.emit("billing.invoice.reminders_completed", summary.clone());
    json::success_response(summary, 200)
}

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

fn on_order_created(state: Arc<BillingState>, ctx: &mut HookContext) {
    let order = ctx.data.clone();
    // Remember the order so `from-order` can build from it later.
    if let Some(order_id) = order.get("id").and_then(|v| v.as_str()) {
        state
            .seen_orders
            .insert(order_id.to_string(), order.clone());
    }
    // Auto-create a draft invoice, unless one already exists for the order.
    let Some(invoice) = state.invoice_from_order_value(&order) else {
        return;
    };
    let order_id = invoice.order_id.clone();
    let already_exists = state.invoices.iter().any(|entry| {
        entry.value().order_id == order_id && entry.value().status != "void"
    });
    if already_exists {
        return;
    }
    state.invoices.insert(invoice.id.clone(), invoice.clone());
    state.emit(
        "invoice.created",
        serde_json::to_value(&invoice).unwrap_or(Value::Null),
    );
}

/// Event names this module listens to (the payments module owns the
/// canonical constants; the literals are repeated here so billing
/// stays decoupled from the payments module).
const PAYMENT_CAPTURED: &str = "payment.captured";
const PAYMENT_REFUNDED: &str = "payment.refunded";

/// The invoice a payment event belongs to: by invoice id when the
/// event carries one, otherwise by order id. Billing auto-creates a
/// draft invoice on `order.created`, so a missing invoice here means
/// the order was never seen by billing, not a lost payment.
fn invoice_for_payment_event(state: &BillingState, ctx: &HookContext) -> Option<Invoice> {
    if let Some(invoice_id) = ctx.get("invoice_id").and_then(|value| value.as_str()) {
        if let Some(invoice) = state
            .invoices
            .get(invoice_id)
            .map(|entry| entry.value().clone())
        {
            return Some(invoice);
        }
    }
    // A payment naming the commercial subscription (renewal or
    // proration) belongs to that subscription's open invoice.
    if let Some(subscription_id) = ctx
        .get("subscription_id")
        .and_then(|value| value.as_str())
    {
        if let Some(invoice) = state
            .invoices
            .iter()
            .find(|entry| {
                entry.value().subscription_id.as_deref() == Some(subscription_id)
                    && !matches!(
                        entry.value().status.as_str(),
                        "paid" | "void" | "credited"
                    )
            })
            .map(|entry| entry.value().clone())
        {
            return Some(invoice);
        }
    }
    let order_id = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .or_else(|| {
            ctx.get("order")
                .and_then(|order| order.get("id"))
                .and_then(|id| id.as_str())
        })?;
    // A renewal invoice and the original order invoice share the
    // order id: prefer the open subscription-linked invoice, so a
    // renewal payment lands on the renewal invoice, not the already
    // paid original.
    if let Some(invoice) = state
        .invoices
        .iter()
        .find(|entry| {
            entry.value().order_id.as_deref() == Some(order_id)
                && entry.value().subscription_id.is_some()
                && !matches!(
                    entry.value().status.as_str(),
                    "paid" | "void" | "credited"
                )
        })
        .map(|entry| entry.value().clone())
    {
        return Some(invoice);
    }
    state
        .invoices
        .iter()
        .find(|entry| entry.value().order_id.as_deref() == Some(order_id))
        .map(|entry| entry.value().clone())
}

/// Dedupe key for a payment event: the refund / transaction id when
/// the event carries one, otherwise the order id plus amount, so a
/// redelivered event never applies twice.
fn payment_event_key(ctx: &HookContext, kind: &str) -> String {
    let id = ctx
        .get("provider_refund_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("transaction_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            ctx.get("provider_transaction_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    match id {
        Some(id) => format!("{kind}:{id}"),
        None => {
            let order_id = ctx
                .get("order_id")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let amount = ctx
                .get("amount_minor")
                .and_then(|value| value.as_i64())
                .map(|amount| amount.to_string())
                .unwrap_or_default();
            format!("{kind}:order:{order_id}:{amount}")
        }
    }
}

/// An approved payment (`payment.captured`) records against the
/// matching invoice: amount paid, remaining balance, and status
/// `paid` or `partially_paid`, with the gateway / transaction
/// reference on the payment record. The existing `invoice.paid`
/// event is emitted when the balance reaches zero, exactly as the
/// manual pay path does. Redeliveries are deduplicated, overpayments
/// are clamped to the outstanding balance, and a currency mismatch is
/// never applied.
fn on_payment_captured(state: Arc<BillingState>, ctx: &mut HookContext) {
    if let Some(status) = ctx.get("status").and_then(|value| value.as_str()) {
        if status != "captured" {
            ctx.add_result(json!({
                "billing": "skipped",
                "reason": format!("payment.captured carries status \"{status}\", not \"captured\""),
            }));
            return;
        }
    }
    let Some(invoice) = invoice_for_payment_event(&state, ctx) else {
        ctx.add_result(json!({
            "billing": "no_invoice",
            "reason": "no invoice exists for this order; billing creates invoices on order.created, so this order was never seen",
        }));
        return;
    };
    if matches!(invoice.status.as_str(), "void" | "credited") {
        ctx.add_result(json!({
            "billing": "skipped",
            "invoice_id": invoice.id,
            "reason": format!("invoice is {}", invoice.status),
        }));
        return;
    }
    let Some(amount_minor) = ctx.get("amount_minor").and_then(|value| value.as_i64()) else {
        ctx.add_result(json!({
            "billing": "skipped",
            "invoice_id": invoice.id,
            "reason": "payment.captured carries no amount",
        }));
        return;
    };
    let currency = ctx
        .get("currency")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if !currency.is_empty() && !currency.eq_ignore_ascii_case(&invoice.currency) {
        ctx.add_result(json!({
            "billing": "skipped",
            "invoice_id": invoice.id,
            "reason": format!(
                "captured currency {currency} does not match invoice currency {}",
                invoice.currency
            ),
        }));
        return;
    }
    let key = payment_event_key(ctx, "capture");
    if state.processed_payment_events.contains_key(&key) {
        ctx.add_result(json!({
            "billing": "duplicate",
            "invoice_id": invoice.id,
            "reason": "this capture was already applied",
        }));
        return;
    }
    let method = ctx
        .get("provider")
        .and_then(|value| value.as_str())
        .unwrap_or("gateway")
        .to_string();
    let reference = ctx
        .get("provider_transaction_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("transaction_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let amount = minor_to_major(amount_minor, &invoice.currency).min(invoice.balance);
    if amount <= 0.0 {
        ctx.add_result(json!({
            "billing": "skipped",
            "invoice_id": invoice.id,
            "reason": "invoice has no outstanding balance",
        }));
        return;
    }
    let payment = InvoicePayment {
        id: new_id(),
        invoice_id: invoice.id.clone(),
        amount,
        method: method.clone(),
        reference,
        recorded_at: now(),
    };
    state.payments.insert(payment.id.clone(), payment.clone());
    let mut stored = state
        .invoices
        .get_mut(&invoice.id)
        .expect("invoice still present");
    stored.amount_paid = round_money(stored.amount_paid + amount);
    recompute_balance(&mut stored);
    if stored.balance == 0.0 {
        stored.status = "paid".to_string();
        stored.paid_at = Some(now());
    } else {
        stored.status = "partially_paid".to_string();
    }
    stored.updated_at = now();
    let fully_paid = stored.status == "paid";
    let invoice_value = serde_json::to_value(&*stored).unwrap_or(Value::Null);
    let payment_value = serde_json::to_value(&payment).unwrap_or(Value::Null);
    drop(stored);
    state
        .processed_payment_events
        .insert(key, now());
    state.emit("invoice.payment.recorded", payment_value);
    if fully_paid {
        state.emit(
            "invoice.paid",
            json!({ "invoice": invoice_value, "method": method }),
        );
    }
    // A fully paid renewal invoice advances the subscription period
    // (see `maybe_renew_subscription`); other invoices never do.
    if let Some(updated_invoice) = state
        .invoices
        .get(&invoice.id)
        .map(|entry| view_invoice(entry.value()))
    {
        state.maybe_renew_subscription(&updated_invoice);
    }
    ctx.add_result(json!({
        "billing": if fully_paid { "paid" } else { "partially_paid" },
        "invoice_id": invoice.id,
        "amount": amount,
    }));
}

/// A completed refund (`payment.refunded` with a `refunded` or
/// `partially_refunded` status) returns money on the matching
/// invoice: a credit note records the refund, `amount_paid` falls by
/// the refunded amount, and an invoice whose paid amount reaches zero
/// reads as `credited` — the invoice status set has no `refunded`
/// state, so `credited` is the existing terminal state for returned
/// money. While the refund is still pending the invoice is left
/// untouched: the pending refund lives in the payments transaction
/// record, and only the refund-completion path finishes it here.
fn on_payment_refunded(state: Arc<BillingState>, ctx: &mut HookContext) {
    let status = ctx
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    match status {
        "refunded" | "partially_refunded" => {}
        "pending" => {
            ctx.add_result(json!({
                "billing": "skipped",
                "reason": "refund is still pending; the invoice is untouched until the refund completes",
            }));
            return;
        }
        other => {
            ctx.add_result(json!({
                "billing": "skipped",
                "reason": format!("refund status \"{other}\" is not a completed refund"),
            }));
            return;
        }
    }
    let Some(invoice) = invoice_for_payment_event(&state, ctx) else {
        ctx.add_result(json!({
            "billing": "no_invoice",
            "reason": "no invoice exists for this order; nothing to credit",
        }));
        return;
    };
    if invoice.status == "void" {
        ctx.add_result(json!({
            "billing": "skipped",
            "invoice_id": invoice.id,
            "reason": "invoice is void",
        }));
        return;
    }
    // Manual store refunds carry no minor-unit amount; the order
    // total is the refunded amount there.
    let refund_major = ctx
        .get("amount_minor")
        .and_then(|value| value.as_i64())
        .map(|amount| minor_to_major(amount, &invoice.currency))
        .or_else(|| {
            ctx.get("order")
                .and_then(|order| order.get("total"))
                .and_then(|total| total.as_f64())
        });
    let Some(refund_major) = refund_major else {
        ctx.add_result(json!({
            "billing": "skipped",
            "invoice_id": invoice.id,
            "reason": "payment.refunded carries no amount",
        }));
        return;
    };
    let key = payment_event_key(ctx, "refund");
    if state.processed_payment_events.contains_key(&key) {
        ctx.add_result(json!({
            "billing": "duplicate",
            "invoice_id": invoice.id,
            "reason": "this refund was already applied",
        }));
        return;
    }
    // A refund can only return money that was paid.
    let refund = round_money(refund_major).min(invoice.amount_paid).max(0.0);
    if refund <= 0.0 {
        ctx.add_result(json!({
            "billing": "skipped",
            "invoice_id": invoice.id,
            "reason": "invoice has no paid amount to refund",
        }));
        return;
    }
    let reason = ctx
        .get("reason")
        .and_then(|value| value.as_str())
        .filter(|reason| !reason.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            ctx.get("provider_refund_id")
                .and_then(|value| value.as_str())
                .map(|id| format!("Refund {id}"))
                .unwrap_or_else(|| "Refund".to_string())
        });
    let credit_note = CreditNote {
        id: new_id(),
        invoice_id: invoice.id.clone(),
        number: state.next_credit_note_number(),
        amount: refund,
        reason,
        created_at: now(),
    };
    state
        .credit_notes
        .insert(credit_note.id.clone(), credit_note.clone());
    let mut stored = state
        .invoices
        .get_mut(&invoice.id)
        .expect("invoice still present");
    stored.credit_total = round_money(stored.credit_total + refund);
    stored.amount_paid = round_money((stored.amount_paid - refund).max(0.0));
    recompute_balance(&mut stored);
    if stored.amount_paid == 0.0 {
        stored.status = "credited".to_string();
    }
    stored.updated_at = now();
    drop(stored);
    state.processed_payment_events.insert(key, now());
    state.emit(
        "invoice.credit_note.created",
        serde_json::to_value(&credit_note).unwrap_or(Value::Null),
    );
    ctx.add_result(json!({
        "billing": "refunded",
        "invoice_id": invoice.id,
        "credit_note_id": credit_note.id,
        "amount": refund,
    }));
}

/// Run the overdue-reminder scan when it is requested through the
/// hook bus (for example by a scheduler job emitting
/// `billing.reminders.run`).
fn on_reminders_run(state: Arc<BillingState>, _ctx: &mut HookContext) {
    let summary = run_reminder_scan(&state);
    state.emit("billing.invoice.reminders_completed", summary);
}

/// The scheduler's daily cleanup also runs the reminder scan, so
/// reminders go out without a dedicated scheduler job kind.
fn on_cleanup_requested(state: Arc<BillingState>, _ctx: &mut HookContext) {
    let summary = run_reminder_scan(&state);
    state.emit("billing.invoice.reminders_completed", summary);
    let subscription_summary = state.run_subscription_scan();
    state.emit(
        "billing.subscriptions.scan_completed",
        subscription_summary,
    );
}

/// Run the subscription lifecycle scan when it is requested through
/// the hook bus (for example by a scheduler job emitting
/// `billing.subscriptions.scan`).
fn on_subscriptions_scan(state: Arc<BillingState>, _ctx: &mut HookContext) {
    let summary = state.run_subscription_scan();
    state.emit("billing.subscriptions.scan_completed", summary);
}

// ---------------------------------------------------------------------------
// Subscription lifecycle hook handlers
// ---------------------------------------------------------------------------

const PAYMENT_FAILED: &str = "payment.failed";
const SUBSCRIPTION_CREATED: &str = "subscription.created";
const SUBSCRIPTION_RENEWED: &str = "subscription.renewed";
const SUBSCRIPTION_TRIAL_ENDED: &str = "subscription.trial_ended";
const SUBSCRIPTION_UPGRADED: &str = "subscription.upgraded";
const SUBSCRIPTION_DOWNGRADED: &str = "subscription.downgraded";
const SUBSCRIPTION_PAST_DUE: &str = "subscription.past_due";
const SUBSCRIPTION_CANCELLED: &str = "subscription.cancelled";

/// Mirror a new subscription from `subscription.created` (emitted by
/// the store when a subscription order is paid).
fn on_subscription_created(state: Arc<BillingState>, ctx: &mut HookContext) {
    match state.sync_subscription_lifecycle(&ctx.data) {
        Some(mirror) => ctx.add_result(json!({
            "billing": "subscription_mirrored",
            "subscription_id": mirror.subscription_id,
            "status": mirror.status,
        })),
        None => ctx.add_result(json!({
            "billing": "skipped",
            "reason": "subscription.created payload lacks the fields a lifecycle mirror needs",
        })),
    }
}

/// Keep the mirror in sync with every later `subscription.*` event.
/// For `subscription.upgraded` / `subscription.downgraded` the
/// proration lines on the event are invoiced: a charge line for the
/// new plan and a credit line for the unused time on the old plan
/// (see `build_proration_invoice`). `no_prorate` changes carry no
/// lines and invoice nothing.
fn on_subscription_synced(state: Arc<BillingState>, ctx: &mut HookContext) {
    let Some(mirror) = state.sync_subscription_lifecycle(&ctx.data) else {
        ctx.add_result(json!({
            "billing": "skipped",
            "reason": "subscription event payload lacks the fields a lifecycle mirror needs",
        }));
        return;
    };
    if !matches!(ctx.event.as_str(), "subscription.upgraded" | "subscription.downgraded") {
        ctx.add_result(json!({
            "billing": "subscription_mirrored",
            "subscription_id": mirror.subscription_id,
            "status": mirror.status,
        }));
        return;
    }
    let proration = ctx.get("proration").cloned().unwrap_or(Value::Null);
    let credit = proration
        .get("credit")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    let charge = proration
        .get("charge")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    if credit <= 0.0 && charge <= 0.0 {
        ctx.add_result(json!({
            "billing": "subscription_mirrored",
            "subscription_id": mirror.subscription_id,
            "status": mirror.status,
            "proration_invoice": Value::Null,
            "reason": "no proration amount to invoice",
        }));
        return;
    }
    let previous_product_id = ctx
        .get("previous_product_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    // The proration invoice is reconciled before it is issued:
    // when validation fails it is stored as a draft instead and
    // no `subscription.proration_invoiced` event is emitted, so
    // no period or balance moves on an unreconciled invoice.
    let invoice = state.build_proration_invoice(&mirror, &previous_product_id, credit, charge);
    let (invoice, validation) = state.validate_proration_invoice(&mirror, invoice);
    let invoice_id = invoice.id.clone();
    let issued = validation.is_ok();
    state.invoices.insert(invoice.id.clone(), invoice.clone());
    state.emit(
        "invoice.created",
        serde_json::to_value(&invoice).unwrap_or(Value::Null),
    );
    if issued {
        state.emit(
            "subscription.proration_invoiced",
            json!({
                "subscription_id": mirror.subscription_id,
                "invoice_id": invoice_id,
                "credit": credit,
                "charge": charge,
            }),
        );
    }
    ctx.add_result(json!({
        "billing": "subscription_mirrored",
        "subscription_id": mirror.subscription_id,
        "status": mirror.status,
        "proration_invoice_id": invoice_id,
        "invoice_status": invoice.status,
        "reconciliation": if issued { Value::Null } else { json!(validation.err().map(|error| error.message)) },
    }));
}

/// Handle a failed payment (`payment.failed`) for a subscription:
/// record the dunning attempt, move the subscription to `past_due`
/// through the store, and request a notification — on every failure
/// and, when the retries are exhausted, a final one. The
/// subscription is found by `subscription_id` when the event names
/// this commercial subscription, otherwise by `order_id` (the
/// payments module's failure carries its own gateway subscription
/// id, which billing cannot resolve directly). Cancellation itself
/// waits for the grace period to expire in the scan; dunning only
/// stops retrying once the attempts are exhausted.
fn on_payment_failed(state: Arc<BillingState>, ctx: &mut HookContext) {
    let subscription_id = ctx
        .get("subscription_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order_id")
                .and_then(|value| value.as_str())
                .and_then(|order_id| {
                    state
                        .subscription_lifecycles
                        .iter()
                        .find(|entry| entry.value().order_id == order_id)
                        .map(|entry| entry.key().clone())
                })
        });
    let Some(subscription_id) = subscription_id else {
        ctx.add_result(json!({
            "billing": "skipped",
            "reason": "payment.failed names no subscription or order billing mirrors",
        }));
        return;
    };
    let Some(mirror) = state
        .subscription_lifecycles
        .get(&subscription_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "subscription_id": subscription_id,
            "reason": "no mirrored subscription for this payment failure",
        }));
        return;
    };
    if mirror.status == "cancelled" {
        ctx.add_result(json!({
            "billing": "skipped",
            "subscription_id": subscription_id,
            "reason": "subscription is cancelled",
        }));
        return;
    }
    let reason = ctx
        .get("reason")
        .and_then(|value| value.as_str())
        .unwrap_or("payment failed")
        .to_string();
    let attempt_number = mirror.dunning_attempts + 1;
    let attempt = DunningAttempt {
        id: new_id(),
        subscription_id: subscription_id.clone(),
        attempt_number,
        reason: reason.clone(),
        amount_minor: ctx.get("amount_minor").and_then(|value| value.as_i64()),
        created_at: now(),
    };
    state
        .dunning_attempts
        .insert(attempt.id.clone(), attempt.clone());
    let next_retry_at = now() + Duration::days(i64::from(mirror.dunning_retry_interval_days));
    if let Some(mut stored) = state.subscription_lifecycles.get_mut(&subscription_id) {
        stored.dunning_attempts = attempt_number;
        stored.last_failure_at = Some(now());
        stored.next_retry_at = Some(next_retry_at);
        stored.updated_at = now();
    }
    state.emit(
        "subscription.lifecycle.apply",
        json!({
            "subscription_id": subscription_id,
            "action": "mark_past_due",
            "reason": reason,
        }),
    );
    state.emit(
        "subscription.payment_failed",
        json!({
            "subscription_id": subscription_id,
            "attempt_number": attempt_number,
            "reason": reason,
        }),
    );
    let exhausted = attempt_number >= mirror.dunning_max_retries;
    if exhausted {
        state.request_notification(
            &mirror.user_id,
            "Subscription payment failed — no retries left",
            &format!(
                "The renewal payment for your subscription (product {}) failed: {reason}. All {} retry attempt(s) are exhausted; the subscription will be cancelled when its grace period ends on {}. Please update your payment method.",
                mirror.product_id,
                mirror.dunning_max_retries,
                (mirror.current_period_end
                    + Duration::days(i64::from(mirror.grace_period_days)))
                .to_rfc3339()
            ),
            "subscription.payment_failed",
        );
    } else {
        state.request_notification(
            &mirror.user_id,
            "Subscription payment failed",
            &format!(
                "The renewal payment for your subscription (product {}) failed: {reason}. Retry attempt {attempt_number} of {} is scheduled for {}. Please update your payment method if the failure was yours.",
                mirror.product_id,
                mirror.dunning_max_retries,
                next_retry_at.to_rfc3339()
            ),
            "subscription.payment_failed",
        );
    }
    ctx.add_result(json!({
        "billing": "dunning_recorded",
        "subscription_id": subscription_id,
        "attempt_number": attempt_number,
        "dunning_exhausted": exhausted,
    }));
}

// ---------------------------------------------------------------------------
// Integration-round listeners: invoice requests, reimbursements,
// digital payouts, SaaS package subscription handoffs
// ---------------------------------------------------------------------------

/// Fulfil `billing.invoice.requested` (admissions registration
/// fees, completed appointments): create one invoice for the
/// requested amount. The requester is identified by the reference
/// key it carries — `application_id` (admissions) or
/// `appointment_id` (appointments) — which becomes the invoice's
/// `order_id`, so `invoice.paid` links the payment back to the
/// requester (admissions matches on `invoice.order_id`). The
/// invoice is created as a `draft`, never `issued`: issuing runs
/// pricing reconciliation in `update_invoice`, and a hook payload
/// carries no tenant context to validate against. The caller's
/// own `reference` string (for example `admissions:<id>`) is not
/// a shared-format reference, so it is kept in the notes instead
/// of the invoice `reference`. A repeat request for the same key
/// returns the existing invoice instead of duplicating it.
fn on_invoice_requested(state: Arc<BillingState>, ctx: &mut HookContext) {
    let reference_key = ctx
        .get("application_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("appointment_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(reference_key) = reference_key.filter(|key| !key.trim().is_empty()) else {
        ctx.add_result(json!({
            "billing": "skipped",
            "reason": "billing.invoice.requested carries no application_id or appointment_id",
        }));
        return;
    };
    if let Some(existing) = state
        .invoices
        .iter()
        .find(|entry| entry.value().order_id.as_deref() == Some(reference_key.as_str()))
        .map(|entry| entry.value().clone())
    {
        ctx.add_result(json!({
            "billing": "invoice_already_created",
            "invoice_id": existing.id,
            "order_id": reference_key,
            "status": existing.status,
        }));
        return;
    }
    let Some(user_id) = ctx
        .get("customer_user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|id| !id.trim().is_empty())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "order_id": reference_key,
            "reason": "billing.invoice.requested carries no customer_user_id",
        }));
        return;
    };
    let Some(amount) = ctx.get("amount").and_then(|value| value.as_f64()) else {
        ctx.add_result(json!({
            "billing": "skipped",
            "order_id": reference_key,
            "reason": "billing.invoice.requested carries no numeric amount",
        }));
        return;
    };
    if !amount.is_finite() || amount < 0.0 {
        ctx.add_result(json!({
            "billing": "skipped",
            "order_id": reference_key,
            "reason": "billing.invoice.requested amount must be a non-negative number",
        }));
        return;
    }
    let currency = ctx
        .get("currency")
        .and_then(|value| value.as_str())
        .unwrap_or("USD")
        .to_string();
    if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_alphabetic()) {
        ctx.add_result(json!({
            "billing": "skipped",
            "order_id": reference_key,
            "reason": "billing.invoice.requested currency must be a 3-letter code",
        }));
        return;
    }
    let description = ctx
        .get("description")
        .and_then(|value| value.as_str())
        .filter(|text| !text.trim().is_empty())
        .unwrap_or("Requested invoice")
        .to_string();
    let caller_reference = ctx
        .get("reference")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let settings = state.settings();
    let (net, tax) = line_tax(
        round_money(amount),
        1,
        settings.default_tax_rate,
        settings.tax_inclusive,
    );
    let line_total = if settings.tax_inclusive {
        round_money(net + tax)
    } else {
        net
    };
    let total = round_money(net + tax);
    let created = now();
    let invoice = Invoice {
        id: new_id(),
        number: state.next_invoice_number(),
        reference: fresh_invoice_reference(&state),
        order_id: Some(reference_key.clone()),
        subscription_id: None,
        user_id,
        company_id: None,
        items: vec![InvoiceItem {
            name: description,
            quantity: 1,
            unit_price: round_money(amount),
            line_total,
            tax_rate: settings.default_tax_rate,
            tax_amount: tax,
        }],
        subtotal: round_money(net),
        tax_total: round_money(tax),
        total,
        currency: currency.to_uppercase(),
        status: "draft".to_string(),
        overdue: false,
        tax_inclusive: settings.tax_inclusive,
        amount_paid: 0.0,
        credit_total: 0.0,
        balance: total,
        issue_date: Some(created),
        due_date: Some(created + Duration::days(i64::from(settings.payment_terms_days))),
        payment_terms_days: Some(settings.payment_terms_days),
        paid_at: None,
        notes: caller_reference,
        reminders_sent: Vec::new(),
        created_at: created,
        updated_at: created,
    };
    state.invoices.insert(invoice.id.clone(), invoice.clone());
    state.emit(
        "invoice.created",
        serde_json::to_value(&invoice).unwrap_or(Value::Null),
    );
    ctx.add_result(json!({
        "billing": "invoice_created",
        "invoice_id": invoice.id,
        "order_id": reference_key,
        "status": "draft",
        "total": invoice.total,
        "currency": invoice.currency,
    }));
}

/// Pay out an approved expense reimbursement
/// (`expenses.reimbursement.requested`
/// `{reimbursement_id, report_id, employee_id, amount_minor,
/// currency, payable_reference}`): record the disbursement in
/// billing's ledger and confirm it with
/// `expenses.reimbursement.paid` `{reimbursement_id, paid_at}`
/// plus the generic `payment.captured` carrying the same
/// `reimbursement_id`, so both of the expenses module's outcome
/// listeners see it. A repeat request for an already-paid
/// reimbursement answers `already_paid` without recording a
/// second disbursement; a failed one may be re-requested (the
/// expenses retry re-emits this event). An invalid payload is
/// answered with `expenses.reimbursement.failed`
/// `{reimbursement_id, reason}` and the report stays `approved`.
fn on_reimbursement_requested(state: Arc<BillingState>, ctx: &mut HookContext) {
    let fail = |state: &Arc<BillingState>,
                ctx: &mut HookContext,
                reimbursement_id: Option<String>,
                reason: &str| {
        if let Some(reimbursement_id) = &reimbursement_id {
            state.emit(
                "expenses.reimbursement.failed",
                json!({
                    "reimbursement_id": reimbursement_id,
                    "reason": reason,
                }),
            );
        }
        ctx.add_result(json!({
            "billing": "reimbursement_failed",
            "reimbursement_id": reimbursement_id,
            "reason": reason,
        }));
    };
    let Some(reimbursement_id) = ctx
        .get("reimbursement_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|id| !id.trim().is_empty())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "reason": "expenses.reimbursement.requested carries no reimbursement_id",
        }));
        return;
    };
    if let Some(existing) = state
        .reimbursement_payouts
        .get(&reimbursement_id)
        .map(|entry| entry.value().clone())
    {
        if existing.status == "paid" {
            ctx.add_result(json!({
                "billing": "reimbursement_already_paid",
                "reimbursement_id": reimbursement_id,
                "paid_at": existing.paid_at,
            }));
            return;
        }
    }
    let amount_minor = ctx
        .get("amount_minor")
        .and_then(|value| value.as_i64());
    let Some(amount_minor) = amount_minor.filter(|amount| *amount > 0) else {
        fail(
            &state,
            ctx,
            Some(reimbursement_id),
            "Reimbursement amount_minor must be a positive integer",
        );
        return;
    };
    let currency = ctx
        .get("currency")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_alphabetic()) {
        fail(
            &state,
            ctx,
            Some(reimbursement_id),
            "Reimbursement currency must be a 3-letter code",
        );
        return;
    }
    let report_id = ctx
        .get("report_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let employee_id = ctx
        .get("employee_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let paid_at = now();
    let attempts = state
        .reimbursement_payouts
        .get(&reimbursement_id)
        .map(|entry| entry.value().attempts + 1)
        .unwrap_or(1);
    let payout = ReimbursementPayout {
        id: reimbursement_id.clone(),
        report_id: report_id.clone(),
        employee_id: employee_id.clone(),
        amount_minor,
        currency: currency.to_uppercase(),
        status: "paid".to_string(),
        failure_reason: None,
        paid_at: Some(paid_at),
        attempts,
        created_at: state
            .reimbursement_payouts
            .get(&reimbursement_id)
            .map(|entry| entry.value().created_at)
            .unwrap_or(paid_at),
        updated_at: paid_at,
    };
    state
        .reimbursement_payouts
        .insert(reimbursement_id.clone(), payout);
    state.emit(
        "expenses.reimbursement.paid",
        json!({
            "reimbursement_id": reimbursement_id,
            "paid_at": paid_at.to_rfc3339(),
        }),
    );
    state.emit(
        "payment.captured",
        json!({
            "reimbursement_id": reimbursement_id,
            "report_id": report_id,
            "employee_id": employee_id,
            "amount_minor": amount_minor,
            "currency": currency.to_uppercase(),
            "status": "captured",
        }),
    );
    ctx.add_result(json!({
        "billing": "reimbursement_paid",
        "reimbursement_id": reimbursement_id,
        "report_id": report_id,
        "amount_minor": amount_minor,
        "currency": currency.to_uppercase(),
        "paid_at": paid_at.to_rfc3339(),
    }));
}

/// Record a digital-product royalty payout request
/// (`store.digital.payout.requested`, the serialized
/// `DigitalPayoutRequest`): billing holds the obligation in its
/// ledger so finance can see it. No money moves on request — the
/// payout is disbursed on `store.digital.payout.approved`.
fn on_digital_payout_requested(state: Arc<BillingState>, ctx: &mut HookContext) {
    let Some(payout_id) = ctx
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|id| !id.trim().is_empty())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "reason": "store.digital.payout.requested carries no payout id",
        }));
        return;
    };
    if state.digital_payouts.contains_key(&payout_id) {
        ctx.add_result(json!({
            "billing": "digital_payout_already_recorded",
            "payout_id": payout_id,
        }));
        return;
    }
    let amount = ctx.get("amount").and_then(|value| value.as_f64());
    let Some(amount) = amount.filter(|value| value.is_finite() && *value > 0.0) else {
        ctx.add_result(json!({
            "billing": "skipped",
            "payout_id": payout_id,
            "reason": "store.digital.payout.requested carries no positive amount",
        }));
        return;
    };
    let currency = ctx
        .get("currency")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_alphabetic()) {
        ctx.add_result(json!({
            "billing": "skipped",
            "payout_id": payout_id,
            "reason": "store.digital.payout.requested currency must be a 3-letter code",
        }));
        return;
    }
    let payout = BillingDigitalPayout {
        id: payout_id.clone(),
        payee: ctx
            .get("payee")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        amount: round_money(amount),
        currency: currency.to_uppercase(),
        royalty_ids: ctx
            .get("royalty_ids")
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        status: "requested".to_string(),
        paid_at: None,
        created_at: now(),
        updated_at: now(),
    };
    state.digital_payouts.insert(payout_id.clone(), payout);
    state.emit(
        "billing.digital.payout.recorded",
        json!({
            "payout_id": payout_id,
            "amount": round_money(amount),
            "currency": currency.to_uppercase(),
            "status": "requested",
        }),
    );
    ctx.add_result(json!({
        "billing": "digital_payout_recorded",
        "payout_id": payout_id,
        "status": "requested",
    }));
}

/// Disburse an approved digital-product royalty payout
/// (`store.digital.payout.approved`, the serialized payout):
/// record the disbursement in billing's ledger and emit
/// `billing.digital.payout.paid`. The store's mark-paid endpoint
/// stays the step that flips the store-side record to `paid`.
/// A payout billing never saw requested is recorded directly
/// from the approval payload instead of being dropped.
fn on_digital_payout_approved(state: Arc<BillingState>, ctx: &mut HookContext) {
    let Some(payout_id) = ctx
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|id| !id.trim().is_empty())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "reason": "store.digital.payout.approved carries no payout id",
        }));
        return;
    };
    if let Some(existing) = state
        .digital_payouts
        .get(&payout_id)
        .map(|entry| entry.value().clone())
    {
        if existing.status == "paid" {
            ctx.add_result(json!({
                "billing": "digital_payout_already_paid",
                "payout_id": payout_id,
                "paid_at": existing.paid_at,
            }));
            return;
        }
    }
    let amount = ctx
        .get("amount")
        .and_then(|value| value.as_f64())
        .filter(|value| value.is_finite() && *value > 0.0)
        .or_else(|| {
            state
                .digital_payouts
                .get(&payout_id)
                .map(|entry| entry.value().amount)
        });
    let Some(amount) = amount else {
        ctx.add_result(json!({
            "billing": "skipped",
            "payout_id": payout_id,
            "reason": "store.digital.payout.approved carries no positive amount",
        }));
        return;
    };
    let currency = ctx
        .get("currency")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            state
                .digital_payouts
                .get(&payout_id)
                .map(|entry| entry.value().currency.clone())
        })
        .unwrap_or_else(|| "USD".to_string());
    let paid_at = now();
    let payout = BillingDigitalPayout {
        id: payout_id.clone(),
        payee: ctx
            .get("payee")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or_else(|| {
                state
                    .digital_payouts
                    .get(&payout_id)
                    .and_then(|entry| entry.value().payee.clone())
            }),
        amount: round_money(amount),
        currency: currency.to_uppercase(),
        royalty_ids: ctx
            .get("royalty_ids")
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .or_else(|| {
                state
                    .digital_payouts
                    .get(&payout_id)
                    .map(|entry| entry.value().royalty_ids.clone())
            })
            .unwrap_or_default(),
        status: "paid".to_string(),
        paid_at: Some(paid_at),
        created_at: state
            .digital_payouts
            .get(&payout_id)
            .map(|entry| entry.value().created_at)
            .unwrap_or(paid_at),
        updated_at: paid_at,
    };
    state.digital_payouts.insert(payout_id.clone(), payout);
    state.emit(
        "billing.digital.payout.paid",
        json!({
            "payout_id": payout_id,
            "amount": round_money(amount),
            "currency": currency.to_uppercase(),
            "paid_at": paid_at.to_rfc3339(),
        }),
    );
    ctx.add_result(json!({
        "billing": "digital_payout_paid",
        "payout_id": payout_id,
        "amount": round_money(amount),
        "currency": currency.to_uppercase(),
        "paid_at": paid_at.to_rfc3339(),
    }));
}

/// Renew a subscription on `billing.subscription.renew_requested`
/// (SaaS packages): issue the renewal invoice for the current
/// period and request its charge, exactly as the lifecycle scan
/// does. A renewal invoice already issued for this period end is
/// not duplicated. When reconciliation does not pass, the invoice
/// is held as a draft and no charge is requested.
fn on_subscription_renew_requested(state: Arc<BillingState>, ctx: &mut HookContext) {
    let Some(subscription_id) = ctx
        .get("subscription_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|id| !id.trim().is_empty())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "reason": "billing.subscription.renew_requested carries no subscription_id",
        }));
        return;
    };
    let Some(mirror) = state
        .subscription_lifecycles
        .get(&subscription_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "subscription_id": subscription_id,
            "reason": "no subscription mirror matches this subscription_id",
        }));
        return;
    };
    if mirror.status == "cancelled" {
        ctx.add_result(json!({
            "billing": "skipped",
            "subscription_id": subscription_id,
            "reason": "subscription is cancelled",
        }));
        return;
    }
    if mirror.renewal_invoice_for_period_end == Some(mirror.current_period_end) {
        ctx.add_result(json!({
            "billing": "renewal_invoice_already_issued",
            "subscription_id": subscription_id,
            "invoice_id": mirror.renewal_invoice_id,
        }));
        return;
    }
    let (invoice, validation) = state.build_validated_renewal_invoice(&mirror);
    let invoice_id = invoice.id.clone();
    let issued = validation.is_ok();
    state.invoices.insert(invoice.id.clone(), invoice.clone());
    state.emit(
        "invoice.created",
        serde_json::to_value(&invoice).unwrap_or(Value::Null),
    );
    if issued {
        if let Some(mut stored) = state.subscription_lifecycles.get_mut(&subscription_id) {
            stored.renewal_invoice_id = Some(invoice_id.clone());
            stored.renewal_invoice_for_period_end = Some(mirror.current_period_end);
            stored.updated_at = now();
        }
        state.request_renewal_charge(&mirror, &invoice_id, false);
    }
    ctx.add_result(json!({
        "billing": if issued { "renewal_invoice_created" } else { "renewal_invoice_held_draft" },
        "subscription_id": subscription_id,
        "invoice_id": invoice_id,
        "invoice_status": invoice.status,
        "reason": if issued { Value::Null } else { json!(validation.err().map(|error| error.message)) },
    }));
}

/// Cancel a subscription on `billing.subscription.cancel_requested`
/// (SaaS packages): billing never drives a status change on its
/// mirror directly — the decision goes out as
/// `subscription.lifecycle.apply` with action `cancel`, the store
/// applies it, and the mirror follows on `subscription.cancelled`.
fn on_subscription_cancel_requested(state: Arc<BillingState>, ctx: &mut HookContext) {
    let Some(subscription_id) = ctx
        .get("subscription_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|id| !id.trim().is_empty())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "reason": "billing.subscription.cancel_requested carries no subscription_id",
        }));
        return;
    };
    let Some(mirror) = state
        .subscription_lifecycles
        .get(&subscription_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "subscription_id": subscription_id,
            "reason": "no subscription mirror matches this subscription_id",
        }));
        return;
    };
    if mirror.status == "cancelled" {
        ctx.add_result(json!({
            "billing": "subscription_already_cancelled",
            "subscription_id": subscription_id,
        }));
        return;
    }
    let reason = ctx
        .get("reason")
        .and_then(|value| value.as_str())
        .unwrap_or("cancelled by package purchase")
        .to_string();
    state.emit(
        "subscription.lifecycle.apply",
        json!({
            "subscription_id": subscription_id,
            "action": "cancel",
            "reason": reason,
        }),
    );
    ctx.add_result(json!({
        "billing": "subscription_cancel_requested",
        "subscription_id": subscription_id,
        "reason": reason,
    }));
}

// ---------------------------------------------------------------------------
// Subscription lifecycle HTTP handlers
// ---------------------------------------------------------------------------

async fn list_subscriptions(
    state: web::Data<Arc<BillingState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<SubscriptionLifecycle> = state
        .subscription_lifecycles
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|mirror| {
            matches_search(
                &format!(
                    "{} {} {}",
                    mirror.subscription_id, mirror.user_id, mirror.product_id
                ),
                &params,
            ) && params
                .filters
                .get("status")
                .is_none_or(|status| mirror.status == *status)
                && params
                    .filters
                    .get("user_id")
                    .is_none_or(|user_id| mirror.user_id == *user_id)
                && params
                    .filters
                    .get("product_id")
                    .is_none_or(|product_id| mirror.product_id == *product_id)
                && params.filters.get("module_id").is_none_or(|module_id| {
                    mirror.module_id.as_deref() == Some(module_id.as_str())
                })
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn get_subscription(
    state: web::Data<Arc<BillingState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<SubscriptionLifecycle> = state
        .subscription_lifecycles
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Subscription not found"));
    json::respond(result, false)
}

async fn list_dunning_attempts(
    state: web::Data<Arc<BillingState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    if !state
        .subscription_lifecycles
        .contains_key(path.as_str())
    {
        return json::respond(
            UnifiedResult::<PageResult<DunningAttempt>>::Err(UnifiedError::not_found(
                "Subscription not found",
            )),
            false,
        );
    }
    let mut items: Vec<DunningAttempt> = state
        .dunning_attempts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|attempt| attempt.subscription_id == path.as_str())
        .collect();
    items.sort_by(|a, b| a.attempt_number.cmp(&b.attempt_number));
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn run_subscription_scan_endpoint(
    state: web::Data<Arc<BillingState>>,
) -> HttpResponse {
    let summary = state.run_subscription_scan();
    state.emit("billing.subscriptions.scan_completed", summary.clone());
    json::success_response(summary, 200)
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct BillingModule {
    state: Arc<BillingState>,
}

impl BillingModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(BillingState::new()),
        }
    }
}

impl Default for BillingModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for BillingModule {
    fn id(&self) -> &'static str {
        "billing"
    }

    fn name(&self) -> &'static str {
        "Billing"
    }

    fn description(&self) -> &'static str {
        "Billing and invoicing: invoices from orders, numbered invoices with taxes and due dates, payments, credit notes, overdue reminders and PDF-ready data, plus the subscription lifecycle engine (renewals, proration, dunning, grace periods, renewal reminders, trial ends). Invoice templates (versioned, per-company, with preview) and invoice printing (print-ready HTML with @page rules, print-job queue via the printing module, email-as-PDF) live here too."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["store"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: crate::core::hooks::HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_order_created(state.clone(), ctx));
        bus.register("order.created", "billing", handler);
        let state = self.state.clone();
        bus.register(
            PAYMENT_CAPTURED,
            "billing",
            Arc::new(move |ctx: &mut HookContext| on_payment_captured(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            PAYMENT_REFUNDED,
            "billing",
            Arc::new(move |ctx: &mut HookContext| on_payment_refunded(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "billing.reminders.run",
            "billing",
            Arc::new(move |ctx: &mut HookContext| on_reminders_run(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "cleanup.requested",
            "billing",
            Arc::new(move |ctx: &mut HookContext| on_cleanup_requested(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "billing.subscriptions.scan",
            "billing",
            Arc::new(move |ctx: &mut HookContext| on_subscriptions_scan(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            SUBSCRIPTION_CREATED,
            "billing",
            Arc::new(move |ctx: &mut HookContext| on_subscription_created(state.clone(), ctx)),
        );
        for event in [
            SUBSCRIPTION_RENEWED,
            SUBSCRIPTION_TRIAL_ENDED,
            SUBSCRIPTION_UPGRADED,
            SUBSCRIPTION_DOWNGRADED,
            SUBSCRIPTION_PAST_DUE,
            SUBSCRIPTION_CANCELLED,
        ] {
            let state = self.state.clone();
            bus.register(
                event,
                "billing",
                Arc::new(move |ctx: &mut HookContext| on_subscription_synced(state.clone(), ctx)),
            );
        }
        let state = self.state.clone();
        bus.register(
            PAYMENT_FAILED,
            "billing",
            Arc::new(move |ctx: &mut HookContext| on_payment_failed(state.clone(), ctx)),
        );
        // --- School fees: academic-module events and invoice
        // sync. Fee charges stay in step with their linked billing
        // invoice through the invoice events below, so gateway
        // captures (`payment.captured`), manual invoice payments
        // and fee payments all converge on the same charge state.
        // Canonical enrollment events: academic emits
        // `academic.enrollment.approved` / `.dropped` — never
        // `academic.student.enrolled` / `.withdrawn`.
        let state = self.state.clone();
        bus.register(
            "academic.enrollment.approved",
            "billing",
            Arc::new(move |ctx: &mut HookContext| {
                on_academic_enrollment_approved(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "academic.enrollment.dropped",
            "billing",
            Arc::new(move |ctx: &mut HookContext| {
                on_academic_enrollment_dropped(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "invoice.payment.recorded",
            "billing",
            Arc::new(move |ctx: &mut HookContext| {
                on_invoice_payment_recorded_for_fee(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "invoice.paid",
            "billing",
            Arc::new(move |ctx: &mut HookContext| {
                on_invoice_paid_for_fee(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "invoice.voided",
            "billing",
            Arc::new(move |ctx: &mut HookContext| {
                on_invoice_voided_for_fee(state.clone(), ctx)
            }),
        );
        // --- Integration round: invoice requests (admissions /
        // appointments), expense reimbursements, digital payouts,
        // SaaS package subscription handoffs.
        let state = self.state.clone();
        bus.register(
            "billing.invoice.requested",
            "billing",
            Arc::new(move |ctx: &mut HookContext| {
                on_invoice_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "expenses.reimbursement.requested",
            "billing",
            Arc::new(move |ctx: &mut HookContext| {
                on_reimbursement_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "store.digital.payout.requested",
            "billing",
            Arc::new(move |ctx: &mut HookContext| {
                on_digital_payout_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "store.digital.payout.approved",
            "billing",
            Arc::new(move |ctx: &mut HookContext| {
                on_digital_payout_approved(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "billing.subscription.renew_requested",
            "billing",
            Arc::new(move |ctx: &mut HookContext| {
                on_subscription_renew_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "billing.subscription.cancel_requested",
            "billing",
            Arc::new(move |ctx: &mut HookContext| {
                on_subscription_cancel_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/billing")
                .route("/invoices", web::get().to(list_invoices))
                .route("/invoices", web::post().to(create_invoice))
                .route("/invoices/from-order", web::post().to(from_order))
                .route("/invoices/{id}", web::get().to(get_invoice))
                .route("/invoices/{id}", web::put().to(update_invoice))
                .route("/invoices/{id}", web::delete().to(delete_invoice))
                .route("/invoices/{id}/pay", web::post().to(pay_invoice))
                .route("/invoices/{id}/void", web::post().to(void_invoice))
                .route("/invoices/{id}/render", web::get().to(render_invoice))
                .route("/invoices/{id}/pdf-data", web::get().to(pdf_data))
                .route("/invoices/{id}/payments", web::get().to(list_payments))
                .route("/invoices/{id}/payments", web::post().to(record_payment))
                .route("/invoices/{id}/credit-notes", web::get().to(list_credit_notes))
                .route("/invoices/{id}/credit-notes", web::post().to(create_credit_note))
                .route("/invoices/{id}/remind", web::post().to(remind_invoice))
                .route("/reminders/run", web::post().to(run_reminders))
                .route("/subscriptions", web::get().to(list_subscriptions))
                .route("/subscriptions/scan", web::post().to(run_subscription_scan_endpoint))
                .route("/subscriptions/{id}", web::get().to(get_subscription))
                .route(
                    "/subscriptions/{id}/dunning",
                    web::get().to(list_dunning_attempts),
                )
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/reference-settings", web::get().to(get_reference_settings))
                .route("/reference-settings", web::put().to(update_reference_settings))
                .route("/overdue", web::get().to(list_overdue))
                .route("/school-fees/types", web::get().to(list_fee_types))
                .route("/school-fees/types", web::post().to(create_fee_type))
                .route("/school-fees/types/{id}", web::get().to(get_fee_type))
                .route("/school-fees/types/{id}", web::put().to(update_fee_type))
                .route(
                    "/school-fees/types/{id}",
                    web::delete().to(delete_fee_type),
                )
                .route("/school-fees/settings", web::get().to(get_fee_settings))
                .route(
                    "/school-fees/settings",
                    web::put().to(update_fee_settings),
                )
                .route("/school-fees/schedules", web::post().to(create_fee_schedule))
                .route("/school-fees/schedules", web::get().to(list_fee_schedules))
                .route(
                    "/school-fees/schedules/{id}",
                    web::get().to(get_fee_schedule),
                )
                .route(
                    "/school-fees/schedules/{id}",
                    web::put().to(update_fee_schedule),
                )
                .route(
                    "/school-fees/schedules/{id}/generate",
                    web::post().to(generate_fee_charges),
                )
                .route("/school-fees/charges", web::get().to(list_fee_charges))
                .route(
                    "/school-fees/charges/{id}",
                    web::get().to(get_fee_charge),
                )
                .route(
                    "/school-fees/charges/{id}/pay",
                    web::post().to(pay_fee_charge),
                )
                .route(
                    "/school-fees/charges/{id}/void",
                    web::post().to(void_fee_charge),
                )
                .route(
                    "/school-fees/charges/{id}/remind",
                    web::post().to(remind_fee_charge),
                )
                .route(
                    "/school-fees/charges/{id}/receipt",
                    web::get().to(get_fee_receipt),
                )
                .route(
                    "/school-fees/charges/{id}/receipt",
                    web::post().to(issue_fee_receipt),
                )
                .route(
                    "/school-fees/reminders/run",
                    web::post().to(run_fee_reminders),
                )
                .route("/school-fees/arrears", web::get().to(list_fee_arrears))
                .route("/school-fees/discounts", web::get().to(list_fee_discounts))
                .route(
                    "/school-fees/discounts",
                    web::post().to(create_fee_discount),
                )
                .route(
                    "/school-fees/discounts/{id}",
                    web::get().to(get_fee_discount),
                )
                .route(
                    "/school-fees/discounts/{id}",
                    web::put().to(update_fee_discount),
                )
                .route(
                    "/school-fees/discounts/{id}",
                    web::delete().to(delete_fee_discount),
                )
                .route("/school-fees/guardians", web::get().to(list_fee_guardians))
                .route(
                    "/school-fees/guardians",
                    web::post().to(create_fee_guardian),
                )
                .route(
                    "/school-fees/guardians/{id}",
                    web::delete().to(delete_fee_guardian),
                )
                .route("/school-fees/my", web::get().to(list_my_fee_charges))
                .route("/invoice-templates", web::get().to(list_invoice_templates))
                .route(
                    "/invoice-templates",
                    web::post().to(create_invoice_template),
                )
                .route(
                    "/invoice-templates/{id}",
                    web::get().to(get_invoice_template),
                )
                .route(
                    "/invoice-templates/{id}",
                    web::put().to(update_invoice_template),
                )
                .route(
                    "/invoice-templates/{id}",
                    web::delete().to(delete_invoice_template),
                )
                .route(
                    "/invoice-templates/{id}/versions",
                    web::get().to(list_invoice_template_versions),
                )
                .route(
                    "/invoice-templates/{id}/versions/{version}/restore",
                    web::post().to(restore_invoice_template_version),
                )
                .route(
                    "/invoice-templates/{id}/set-default",
                    web::post().to(set_default_invoice_template),
                )
                .route(
                    "/invoice-templates/{id}/preview",
                    web::get().to(preview_invoice_template),
                )
                .route("/invoices/{id}/print", web::post().to(print_invoice))
                .route(
                    "/invoices/{id}/print-ready",
                    web::get().to(print_ready_invoice),
                )
                .route(
                    "/invoices/{id}/print-jobs",
                    web::get().to(list_invoice_print_jobs),
                )
                .route(
                    "/invoices/{id}/email-pdf",
                    web::post().to(email_invoice_pdf),
                )
                .route(
                    "/print-jobs/{job_id}/reprint",
                    web::post().to(reprint_invoice_print_job),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/billing/invoices", owner: "billing", description: "List invoices with pagination and filters." },
            RouteInfo { method: "POST", path: "/api/v1/billing/invoices", owner: "billing", description: "Create an invoice with per-line taxes." },
            RouteInfo { method: "POST", path: "/api/v1/billing/invoices/from-order", owner: "billing", description: "Build an invoice from a known order." },
            RouteInfo { method: "GET", path: "/api/v1/billing/invoices/{id}", owner: "billing", description: "Get one invoice." },
            RouteInfo { method: "PUT", path: "/api/v1/billing/invoices/{id}", owner: "billing", description: "Update an open invoice." },
            RouteInfo { method: "DELETE", path: "/api/v1/billing/invoices/{id}", owner: "billing", description: "Delete a draft invoice." },
            RouteInfo { method: "POST", path: "/api/v1/billing/invoices/{id}/pay", owner: "billing", description: "Pay an invoice in full and record the payment." },
            RouteInfo { method: "POST", path: "/api/v1/billing/invoices/{id}/void", owner: "billing", description: "Void an unpaid invoice." },
            RouteInfo { method: "GET", path: "/api/v1/billing/invoices/{id}/render", owner: "billing", description: "Render an invoice as HTML." },
            RouteInfo { method: "GET", path: "/api/v1/billing/invoices/{id}/pdf-data", owner: "billing", description: "Full structured invoice payload for a PDF renderer." },
            RouteInfo { method: "GET", path: "/api/v1/billing/invoices/{id}/payments", owner: "billing", description: "List payments recorded against an invoice." },
            RouteInfo { method: "POST", path: "/api/v1/billing/invoices/{id}/payments", owner: "billing", description: "Record a payment (full or partial) against an invoice." },
            RouteInfo { method: "GET", path: "/api/v1/billing/invoices/{id}/credit-notes", owner: "billing", description: "List credit notes issued against an invoice." },
            RouteInfo { method: "POST", path: "/api/v1/billing/invoices/{id}/credit-notes", owner: "billing", description: "Create a credit note against an invoice." },
            RouteInfo { method: "POST", path: "/api/v1/billing/invoices/{id}/remind", owner: "billing", description: "Send one overdue reminder for an invoice (emits billing.invoice.reminder)." },
            RouteInfo { method: "POST", path: "/api/v1/billing/reminders/run", owner: "billing", description: "Run the overdue-reminder scan for all open invoices." },
            RouteInfo { method: "GET", path: "/api/v1/billing/subscriptions", owner: "billing", description: "List subscription lifecycle mirrors with pagination and filters." },
            RouteInfo { method: "POST", path: "/api/v1/billing/subscriptions/scan", owner: "billing", description: "Run the subscription lifecycle scan (trial ends, renewal reminders and invoices, dunning retries, grace expiry, period-end cancellations)." },
            RouteInfo { method: "GET", path: "/api/v1/billing/subscriptions/{id}", owner: "billing", description: "Get one subscription lifecycle mirror." },
            RouteInfo { method: "GET", path: "/api/v1/billing/subscriptions/{id}/dunning", owner: "billing", description: "List recorded dunning attempts for a subscription." },
            RouteInfo { method: "GET", path: "/api/v1/billing/settings", owner: "billing", description: "Read billing settings (numbering, taxes, terms, reminders)." },
            RouteInfo { method: "PUT", path: "/api/v1/billing/settings", owner: "billing", description: "Update billing settings." },
            RouteInfo { method: "GET", path: "/api/v1/billing/reference-settings", owner: "billing", description: "Read payment reference settings (default prefix, per-product prefixes)." },
            RouteInfo { method: "PUT", path: "/api/v1/billing/reference-settings", owner: "billing", description: "Update payment reference settings (capability billing.reference:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/billing/overdue", owner: "billing", description: "List overdue invoices." },
            RouteInfo { method: "GET", path: "/api/v1/billing/invoice-templates", owner: "billing", description: "List invoice templates with pagination and filters (company, default flag)." },
            RouteInfo { method: "POST", path: "/api/v1/billing/invoice-templates", owner: "billing", description: "Create an invoice template (header/footer, logo reference, colours, columns, tax breakdown, payment instructions)." },
            RouteInfo { method: "GET", path: "/api/v1/billing/invoice-templates/{id}", owner: "billing", description: "Get one invoice template." },
            RouteInfo { method: "PUT", path: "/api/v1/billing/invoice-templates/{id}", owner: "billing", description: "Update an invoice template; the previous version is archived and the version number bumps." },
            RouteInfo { method: "DELETE", path: "/api/v1/billing/invoice-templates/{id}", owner: "billing", description: "Delete an invoice template and its version history." },
            RouteInfo { method: "GET", path: "/api/v1/billing/invoice-templates/{id}/versions", owner: "billing", description: "List the version history of an invoice template." },
            RouteInfo { method: "POST", path: "/api/v1/billing/invoice-templates/{id}/versions/{version}/restore", owner: "billing", description: "Restore an old template version as a new version." },
            RouteInfo { method: "POST", path: "/api/v1/billing/invoice-templates/{id}/set-default", owner: "billing", description: "Set an invoice template as the default for one company." },
            RouteInfo { method: "GET", path: "/api/v1/billing/invoice-templates/{id}/preview", owner: "billing", description: "Preview a template rendered with a real invoice or with sample invoice data." },
            RouteInfo { method: "POST", path: "/api/v1/billing/invoices/{id}/print", owner: "billing", description: "Print an invoice: render the print-ready document and queue a print job through the printing module." },
            RouteInfo { method: "GET", path: "/api/v1/billing/invoices/{id}/print-ready", owner: "billing", description: "Get the print-ready HTML document for an invoice, with @page rules for the chosen paper size." },
            RouteInfo { method: "GET", path: "/api/v1/billing/invoices/{id}/print-jobs", owner: "billing", description: "List the print jobs of one invoice (served by the printing module)." },
            RouteInfo { method: "POST", path: "/api/v1/billing/invoices/{id}/email-pdf", owner: "billing", description: "Email an invoice as a print-ready document through mail.send_requested." },
            RouteInfo { method: "POST", path: "/api/v1/billing/print-jobs/{job_id}/reprint", owner: "billing", description: "Queue a fresh copy of a print job through the printing module." },
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/types", owner: "billing", description: "List school fee types (admin-editable definitions)." },
            RouteInfo { method: "POST", path: "/api/v1/billing/school-fees/types", owner: "billing", description: "Create a school fee type (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/types/{id}", owner: "billing", description: "Get one school fee type." },
            RouteInfo { method: "PUT", path: "/api/v1/billing/school-fees/types/{id}", owner: "billing", description: "Update a school fee type (manager)." },
            RouteInfo { method: "DELETE", path: "/api/v1/billing/school-fees/types/{id}", owner: "billing", description: "Delete a school fee type with no schedules (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/settings", owner: "billing", description: "Read school fee settings (configurable defaults)." },
            RouteInfo { method: "PUT", path: "/api/v1/billing/school-fees/settings", owner: "billing", description: "Update school fee settings (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/billing/school-fees/schedules", owner: "billing", description: "Create a per-student fee schedule (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/schedules", owner: "billing", description: "List fee schedules with pagination and filters (scoped to the caller)." },
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/schedules/{id}", owner: "billing", description: "Get one fee schedule (scoped to the caller)." },
            RouteInfo { method: "PUT", path: "/api/v1/billing/school-fees/schedules/{id}", owner: "billing", description: "Update a fee schedule (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/billing/school-fees/schedules/{id}/generate", owner: "billing", description: "Generate a fee charge and its billing invoice for one period (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/charges", owner: "billing", description: "List fee charges with pagination, filters and aging (scoped to the caller)." },
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/charges/{id}", owner: "billing", description: "Get one fee charge (scoped to the caller)." },
            RouteInfo { method: "POST", path: "/api/v1/billing/school-fees/charges/{id}/pay", owner: "billing", description: "Pay a fee charge through its billing invoice (manager, or the student / linked guardian)." },
            RouteInfo { method: "POST", path: "/api/v1/billing/school-fees/charges/{id}/void", owner: "billing", description: "Void a fee charge and its billing invoice (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/billing/school-fees/charges/{id}/remind", owner: "billing", description: "Send one fee reminder for a charge (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/charges/{id}/receipt", owner: "billing", description: "Get the latest issued receipt and print payload for a charge (scoped to the caller)." },
            RouteInfo { method: "POST", path: "/api/v1/billing/school-fees/charges/{id}/receipt", owner: "billing", description: "Issue a fee receipt and request printing (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/billing/school-fees/reminders/run", owner: "billing", description: "Run the fee-reminder scan for due and overdue charges (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/arrears", owner: "billing", description: "Per-student arrears and aging summary (scoped to the caller)." },
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/discounts", owner: "billing", description: "List fee discounts and scholarships (scoped to the caller)." },
            RouteInfo { method: "POST", path: "/api/v1/billing/school-fees/discounts", owner: "billing", description: "Create a per-student discount or scholarship (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/discounts/{id}", owner: "billing", description: "Get one fee discount (scoped to the caller)." },
            RouteInfo { method: "PUT", path: "/api/v1/billing/school-fees/discounts/{id}", owner: "billing", description: "Update or approve a fee discount (manager)." },
            RouteInfo { method: "DELETE", path: "/api/v1/billing/school-fees/discounts/{id}", owner: "billing", description: "Delete a fee discount (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/guardians", owner: "billing", description: "List guardian links for a student (manager, or the guardian)." },
            RouteInfo { method: "POST", path: "/api/v1/billing/school-fees/guardians", owner: "billing", description: "Link a guardian to a student (manager)." },
            RouteInfo { method: "DELETE", path: "/api/v1/billing/school-fees/guardians/{id}", owner: "billing", description: "Remove a guardian link (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/my", owner: "billing", description: "Fee charges for the caller: the student's own, or their children's for a guardian." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(BillingModule::new()));
}

// ---------------------------------------------------------------------------
// School fees
// ---------------------------------------------------------------------------
//
// Per-student school fee billing, built on top of the billing
// invoice flow instead of duplicating it. A fee charge always has
// one linked billing invoice (except a fully discounted charge,
// whose net total is zero and which therefore needs no invoice):
// payments are recorded against that invoice through the normal
// invoice payment path (`record_payment`, `pay_invoice`, or the
// gateway `payment.captured` event), and the charge mirrors the
// invoice through the `invoice.payment.recorded`, `invoice.paid`
// and `invoice.voided` handlers below. Receipts never call a
// printing module directly: issuing a receipt emits
// `school_fees.receipt.print_requested` for the printing module
// to consume.
//
// Money in this section is integer minor units (`i64`) with a
// currency code, never floats. The linked invoice carries the same
// amount as major-unit floats only because the pre-existing
// invoice type does; conversion happens at that boundary with
// `minor_to_major` / `major_to_minor`.
//
// Student identity is the academic module's `user_id`: this
// section never stores its own student records. Program, level
// and academic period are opaque codes owned by the academic
// module and referenced here, never defined here. Fee types,
// schedules, discounts and reminder/aging rules are all data:
// fee types and settings are admin-editable, so no fee, schedule
// rule or category is baked into code.
//
// Visibility: a caller with `school_fees.manage` sees everything.
// Anyone else sees only their own charges (as the student) or
// their linked children's charges (as a guardian, through the
// guardian links below). Every endpoint enforces this server-side.

const PERM_SCHOOL_FEES_VIEW: &str = "school_fees.view";
const PERM_SCHOOL_FEES_MANAGE: &str = "school_fees.manage";

/// School fee settings. All values are configurable defaults:
/// nothing in this section treats them as fixed rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchoolFeeSettings {
    /// Default currency for new fee types (ISO code).
    pub currency: String,
    /// Days after an academic period starts when an auto-assigned
    /// schedule first falls due.
    pub auto_assign_due_in_days: u32,
    /// Reminder schedule: days before the due date a reminder is due.
    pub reminder_before_days: Vec<i32>,
    /// Reminder schedule: days after the due date a reminder is due.
    pub reminder_after_days: Vec<i32>,
    /// Aging bucket cut-offs in days overdue, ascending.
    /// With `[30, 60, 90]` the buckets are `1-30`, `31-60`,
    /// `61-90` and `over_90`.
    pub aging_bucket_days: Vec<i32>,
    /// Allowed fee-type categories. Fee types may only use a
    /// category from this list; the list itself is editable.
    pub fee_categories: Vec<String>,
    /// Allowed billing periods for fee types (for example
    /// `monthly`, `per_term`, `one_off`).
    pub billing_periods: Vec<String>,
    /// Prefix for generated receipt numbers.
    pub receipt_prefix: String,
    /// When true, a new discount starts unapproved and a manager
    /// must approve it before it applies to charges.
    pub discount_requires_approval: bool,
}

impl Default for SchoolFeeSettings {
    fn default() -> Self {
        Self {
            currency: "USD".to_string(),
            auto_assign_due_in_days: 30,
            reminder_before_days: vec![7, 3, 1],
            reminder_after_days: vec![1, 7, 14],
            aging_bucket_days: vec![30, 60, 90],
            fee_categories: vec![
                "tuition".to_string(),
                "registration".to_string(),
                "examination".to_string(),
                "activities".to_string(),
                "transport".to_string(),
                "meals".to_string(),
                "other".to_string(),
            ],
            billing_periods: vec![
                "monthly".to_string(),
                "per_term".to_string(),
                "one_off".to_string(),
            ],
            receipt_prefix: "RCPT".to_string(),
            discount_requires_approval: true,
        }
    }
}

/// One admin-editable fee type definition. Fee types are data, not
/// code: registration, examination and activity fees are just rows
/// in this table, created by an administrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchoolFeeType {
    pub id: String,
    /// Stable code, unique across fee types (for example
    /// `monthly-tuition`).
    pub code: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Category, validated against the settings category list.
    pub category: String,
    /// Billing period, validated against the settings period list.
    pub billing_period: String,
    /// Default gross amount in minor units, used when a schedule
    /// does not set its own amount.
    pub default_amount_minor: i64,
    pub currency: String,
    /// When true, enrolment (`academic.enrollment.approved`)
    /// creates a schedule for this fee type automatically.
    #[serde(default)]
    pub auto_assign_on_enrolment: bool,
    #[serde(default = "default_true")]
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn default_true() -> bool {
    true
}

/// One per-student fee schedule: which fees apply to a student,
/// for which program, level and academic period, at what amount,
/// and when they fall due.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudentFeeSchedule {
    pub id: String,
    /// Academic module identity of the student.
    pub student_user_id: String,
    /// Program code owned by the academic module.
    pub program_code: String,
    /// Level code owned by the academic module.
    pub level_code: String,
    /// Academic period id owned by the academic module.
    pub academic_period_id: String,
    pub fee_type_id: String,
    /// Gross amount in minor units for one generated charge.
    pub amount_minor: i64,
    pub currency: String,
    pub due_date: DateTime<Utc>,
    /// `active`, `paused` or `cancelled`.
    pub status: String,
    #[serde(default)]
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One generated fee charge: a single amount owed for one period,
/// linked to one billing invoice (unless fully discounted).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeCharge {
    pub id: String,
    pub schedule_id: String,
    pub student_user_id: String,
    pub fee_type_id: String,
    pub academic_period_id: String,
    /// Period label supplied at generation (for example `2026-11`).
    pub period_label: String,
    /// Gross amount in minor units, before discounts.
    pub amount_minor: i64,
    /// Discount applied in minor units.
    #[serde(default)]
    pub discount_minor: i64,
    /// Net amount owed in minor units (`amount - discount`).
    pub total_minor: i64,
    /// Amount paid so far in minor units, mirrored from the
    /// linked invoice.
    #[serde(default)]
    pub amount_paid_minor: i64,
    pub currency: String,
    /// Linked billing invoice, if any. `None` only for a fully
    /// discounted charge, which has nothing to collect.
    #[serde(default)]
    pub invoice_id: Option<String>,
    /// Discount ids applied to this charge.
    #[serde(default)]
    pub discount_ids: Vec<String>,
    pub due_date: DateTime<Utc>,
    /// Base status: `open`, `partially_paid`, `paid` or `void`.
    /// Reads report `overdue` for an open charge past its due
    /// date (see `view_fee_charge`).
    pub status: String,
    /// True when the charge is past its due date with a balance
    /// outstanding. Derived on read, not stored as source of truth.
    #[serde(default)]
    pub overdue: bool,
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One per-student discount or scholarship.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeDiscount {
    pub id: String,
    pub student_user_id: String,
    /// Fee type this discount applies to, or `None` for all types.
    #[serde(default)]
    pub fee_type_id: Option<String>,
    /// `percentage` or `fixed`.
    pub kind: String,
    /// Percentage in basis points (2500 means 25.00%). Only used
    /// when `kind` is `percentage`.
    #[serde(default)]
    pub percent_basis_points: i64,
    /// Fixed discount in minor units. Only used when `kind` is
    /// `fixed`.
    #[serde(default)]
    pub amount_minor: i64,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    /// Whether the discount is approved and may apply to charges.
    /// See `discount_requires_approval` in the settings.
    #[serde(default)]
    pub approved: bool,
    #[serde(default)]
    pub approved_by: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    /// `active` or `revoked`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One issued receipt for a fee charge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeReceipt {
    pub id: String,
    /// Human-readable receipt number, for example `RCPT-000001`.
    pub number: String,
    pub charge_id: String,
    pub student_user_id: String,
    /// Receipt amount in minor units (amount paid so far).
    pub amount_minor: i64,
    pub currency: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub reference: Option<String>,
    pub issued_by: String,
    pub issued_at: DateTime<Utc>,
}

/// One guardian-to-student link. Guardians see only the students
/// linked to them here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeGuardianLink {
    pub id: String,
    pub guardian_user_id: String,
    pub student_user_id: String,
    /// Relationship label, for example `parent` or `guardian`.
    #[serde(default)]
    pub relationship: String,
    #[serde(default = "default_true")]
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

/// One fee reminder already sent, so the scan never sends the same
/// reminder twice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeReminder {
    pub charge_id: String,
    /// Days relative to the due date: positive means days before
    /// due, negative means days after due, `0` means a manual
    /// reminder.
    pub offset_days: i32,
    pub recipient_user_id: String,
    pub sent_at: DateTime<Utc>,
}

/// One aging bucket in an arrears summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArrearsBucket {
    pub bucket: String,
    pub amount_minor: i64,
    pub charge_count: u32,
}

/// Per-student arrears and aging summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudentArrears {
    pub student_user_id: String,
    pub total_overdue_minor: i64,
    pub currency: String,
    pub charge_count: u32,
    pub oldest_due_date: Option<DateTime<Utc>>,
    pub buckets: Vec<ArrearsBucket>,
}

// --- School fee helpers -------------------------------------------------

impl BillingState {
    fn fee_settings(&self) -> SchoolFeeSettings {
        self.fee_settings
            .read()
            .expect("school fee settings lock poisoned")
            .clone()
    }

    fn next_fee_receipt_number(&self) -> String {
        let prefix = self.fee_settings().receipt_prefix;
        let seq = self.fee_receipt_seq.fetch_add(1, Ordering::SeqCst);
        format!("{prefix}-{seq:06}")
    }
}

/// Convert major units to integer minor units.
fn major_to_minor(amount: f64, currency: &str) -> i64 {
    if is_zero_decimal_currency(currency) {
        amount.round() as i64
    } else {
        (amount * 100.0).round() as i64
    }
}

/// Format integer minor units as a decimal string (no floats in the
/// receipt payload).
fn minor_to_decimal_string(amount_minor: i64, currency: &str) -> String {
    if is_zero_decimal_currency(currency) {
        format!("{amount_minor}")
    } else {
        let sign = if amount_minor < 0 { "-" } else { "" };
        let absolute = amount_minor.abs();
        format!("{sign}{}.{:02}", absolute / 100, absolute % 100)
    }
}

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the e-learning module.
struct FeeCaller {
    actor: String,
    role: String,
}

fn fee_caller(req: &HttpRequest) -> UnifiedResult<FeeCaller> {
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
    Ok(FeeCaller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the e-learning module).
fn fee_check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn fee_require_permission(
    ctx: &AppContext,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if fee_check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

fn fee_require_view(ctx: &AppContext, caller: &FeeCaller) -> UnifiedResult<()> {
    fee_require_permission(ctx, &caller.role, PERM_SCHOOL_FEES_VIEW)
}

fn fee_require_manage(ctx: &AppContext, caller: &FeeCaller) -> UnifiedResult<()> {
    fee_require_permission(ctx, &caller.role, PERM_SCHOOL_FEES_MANAGE)
}

fn fee_is_manager(ctx: &AppContext, caller: &FeeCaller) -> bool {
    fee_check_capability(ctx, &caller.role, PERM_SCHOOL_FEES_MANAGE)
}

/// Student ids the caller may see: the caller's own id plus the
/// students linked to them as a guardian. Managers bypass this
/// and see everything.
fn fee_visible_student_ids(state: &Arc<BillingState>, caller: &FeeCaller) -> Vec<String> {
    let mut ids = vec![caller.actor.clone()];
    for entry in state.fee_guardians.iter() {
        let link = entry.value();
        if link.active
            && link.guardian_user_id == caller.actor
            && !ids.contains(&link.student_user_id)
        {
            ids.push(link.student_user_id.clone());
        }
    }
    ids
}

fn fee_can_view_student(
    ctx: &AppContext,
    state: &Arc<BillingState>,
    caller: &FeeCaller,
    student_user_id: &str,
) -> bool {
    fee_is_manager(ctx, caller)
        || fee_visible_student_ids(state, caller)
            .iter()
            .any(|id| id == student_user_id)
}

fn fee_require_charge_access(
    ctx: &AppContext,
    state: &Arc<BillingState>,
    caller: &FeeCaller,
    charge: &FeeCharge,
) -> UnifiedResult<()> {
    if fee_can_view_student(ctx, state, caller, &charge.student_user_id) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Fee charge belongs to a different student",
        ))
    }
}

fn fee_require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be empty"
        )));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn fee_optional_text(
    value: &Option<String>,
    field: &str,
    max: usize,
) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Ok(Some(fee_require_text(raw, field, max)?)),
        None => Ok(None),
    }
}

fn fee_require_currency(value: &str) -> UnifiedResult<String> {
    let code = fee_require_text(value, "currency", 3)?;
    if code.len() != 3 || !code.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code",
        ));
    }
    Ok(code.to_ascii_uppercase())
}

/// True when a charge is past its due date with a balance still
/// outstanding. Paid and void charges are never overdue.
fn fee_charge_is_overdue(charge: &FeeCharge, at: DateTime<Utc>) -> bool {
    matches!(charge.status.as_str(), "open" | "partially_paid")
        && charge.total_minor - charge.amount_paid_minor > 0
        && charge.due_date < at
}

/// The status callers see: an unpaid charge past its due date reads
/// as `overdue`. The stored status is left untouched.
fn fee_charge_effective_status(charge: &FeeCharge, at: DateTime<Utc>) -> String {
    if fee_charge_is_overdue(charge, at) {
        "overdue".to_string()
    } else {
        charge.status.clone()
    }
}

/// A copy of a charge with the derived `overdue` flag and effective
/// status filled in. All read endpoints return this view.
fn view_fee_charge(charge: &FeeCharge) -> FeeCharge {
    let mut view = charge.clone();
    view.overdue = fee_charge_is_overdue(charge, now());
    if view.overdue {
        view.status = "overdue".to_string();
    }
    view
}

/// Days a charge is overdue (zero when it is not yet due).
fn fee_days_overdue(charge: &FeeCharge, at: DateTime<Utc>) -> i64 {
    if charge.due_date >= at {
        0
    } else {
        (at.date_naive() - charge.due_date.date_naive()).num_days()
    }
}

/// Aging bucket label for a number of days overdue, from the
/// configured cut-offs. Cut-offs `[30, 60, 90]` give `1-30`,
/// `31-60`, `61-90` and `over_90`.
fn fee_aging_bucket(cutoffs: &[i32], days_overdue: i64) -> String {
    let mut lower = 0i64;
    for &cutoff in cutoffs {
        if days_overdue <= i64::from(cutoff) {
            return format!("{}-{}", lower + 1, cutoff);
        }
        lower = i64::from(cutoff);
    }
    let last = cutoffs.last().copied().unwrap_or(0);
    format!("over_{last}")
}

/// Discounts that may apply to a new charge for this student and
/// fee type right now: active, approved and inside their validity
/// period.
fn fee_applicable_discounts(
    state: &Arc<BillingState>,
    student_user_id: &str,
    fee_type_id: &str,
    at: DateTime<Utc>,
) -> Vec<FeeDiscount> {
    state
        .fee_discounts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|discount| {
            discount.student_user_id == student_user_id
                && discount.status == "active"
                && discount.approved
                && discount
                    .fee_type_id
                    .as_deref()
                    .is_none_or(|id| id == fee_type_id)
                && discount.valid_from.is_none_or(|from| from <= at)
                && discount.valid_to.is_none_or(|to| to >= at)
        })
        .collect()
}

/// Discount amount in minor units for one gross amount. Percentage
/// discounts use basis points; fixed discounts are capped at the
/// gross amount so a charge can never go negative.
fn fee_discount_amount(gross_minor: i64, discount: &FeeDiscount) -> i64 {
    match discount.kind.as_str() {
        "percentage" => {
            (gross_minor * discount.percent_basis_points.clamp(0, 10_000)) / 10_000
        }
        _ => discount.amount_minor.max(0).min(gross_minor),
    }
}

/// Reminder offsets that fall due today for one charge, in days
/// relative to the due date (positive = days before due, negative =
/// days after due). Mirrors the invoice reminder schedule.
fn fee_reminders_due(
    charge: &FeeCharge,
    settings: &SchoolFeeSettings,
    at: DateTime<Utc>,
) -> Vec<i32> {
    if charge.total_minor - charge.amount_paid_minor <= 0
        || matches!(charge.status.as_str(), "paid" | "void")
    {
        return Vec::new();
    }
    let today = at.date_naive();
    let due_day = charge.due_date.date_naive();
    let mut due_offsets: Vec<i32> = Vec::new();
    for &days in &settings.reminder_before_days {
        if days > 0 && today == due_day - Duration::days(i64::from(days)) {
            due_offsets.push(days);
        }
    }
    for &days in &settings.reminder_after_days {
        if days > 0 && today == due_day + Duration::days(i64::from(days)) {
            due_offsets.push(-days);
        }
    }
    due_offsets
}

/// Recipients for a fee reminder: the student plus every active
/// linked guardian.
fn fee_reminder_recipients(state: &Arc<BillingState>, charge: &FeeCharge) -> Vec<String> {
    let mut recipients = vec![charge.student_user_id.clone()];
    for entry in state.fee_guardians.iter() {
        let link = entry.value();
        if link.active
            && link.student_user_id == charge.student_user_id
            && !recipients.contains(&link.guardian_user_id)
        {
            recipients.push(link.guardian_user_id.clone());
        }
    }
    recipients
}

/// Build and store the billing invoice for a fee charge. The
/// invoice is the payment record: every payment against the charge
/// is a payment against this invoice.
/// NOTE: this invoice is issued directly, without pricing
/// reconciliation — the charge carries no tenant or company id to
/// validate against, so `validate_invoice_with_pricing` cannot
/// run here. It is the one remaining unenforced issue path (see
/// the integration report); it becomes enforceable once fee
/// charges carry a tenant id.
fn build_fee_invoice(
    state: &Arc<BillingState>,
    charge: &FeeCharge,
    fee_type: &SchoolFeeType,
    period_label: &str,
) -> Invoice {
    let total = minor_to_major(charge.total_minor, &charge.currency);
    let invoice = Invoice {
        id: new_id(),
        number: state.next_invoice_number(),
        reference: fresh_invoice_reference(&**state),
        order_id: None,
        subscription_id: None,
        user_id: charge.student_user_id.clone(),
        company_id: None,
        items: vec![InvoiceItem {
            name: format!("{} ({})", fee_type.name, period_label),
            quantity: 1,
            unit_price: total,
            line_total: total,
            tax_rate: 0.0,
            tax_amount: 0.0,
        }],
        subtotal: total,
        tax_total: 0.0,
        total,
        currency: charge.currency.clone(),
        status: "issued".to_string(),
        overdue: false,
        tax_inclusive: false,
        amount_paid: 0.0,
        credit_total: 0.0,
        balance: total,
        issue_date: Some(now()),
        due_date: Some(charge.due_date),
        payment_terms_days: None,
        paid_at: None,
        notes: Some(format!("School fee charge {}", charge.id)),
        reminders_sent: Vec::new(),
        created_at: now(),
        updated_at: now(),
    };
    state.invoices.insert(invoice.id.clone(), invoice.clone());
    invoice
}

/// Record a payment against a fee charge's linked invoice,
/// mirroring the `record_payment` validation and bookkeeping
/// (balance cap, partial/full status, `invoice.payment.recorded`
/// and `invoice.paid` events). The charge itself is updated by
/// the invoice-event sync handlers, which run synchronously inside
/// the emits below.
fn record_fee_invoice_payment(
    state: &Arc<BillingState>,
    charge: &FeeCharge,
    amount_minor: i64,
    method: &str,
    reference: Option<String>,
) -> UnifiedResult<InvoicePayment> {
    let invoice_id = charge.invoice_id.clone().ok_or_else(|| {
        UnifiedError::unprocessable("Fee charge has no linked invoice to pay")
    })?;
    let mut invoice = state
        .invoices
        .get_mut(&invoice_id)
        .ok_or_else(|| UnifiedError::not_found("Linked invoice not found"))?;
    if invoice.status == "paid" {
        return Err(UnifiedError::conflict("Invoice is already paid"));
    }
    if invoice.status == "void" {
        return Err(UnifiedError::unprocessable(
            "A void invoice cannot be paid",
        ));
    }
    if invoice.status == "credited" {
        return Err(UnifiedError::unprocessable(
            "A credited invoice cannot be paid",
        ));
    }
    let amount = minor_to_major(amount_minor, &charge.currency);
    if amount > invoice.balance {
        return Err(UnifiedError::unprocessable(format!(
            "Payment of {} exceeds the outstanding balance of {}",
            minor_to_decimal_string(amount_minor, &charge.currency),
            minor_to_decimal_string(
                major_to_minor(invoice.balance, &charge.currency),
                &charge.currency
            ),
        )));
    }
    let payment = InvoicePayment {
        id: new_id(),
        invoice_id: invoice.id.clone(),
        amount,
        method: method.to_string(),
        reference,
        recorded_at: now(),
    };
    state.payments.insert(payment.id.clone(), payment.clone());
    invoice.amount_paid = round_money(invoice.amount_paid + amount);
    recompute_balance(&mut invoice);
    if invoice.balance == 0.0 {
        invoice.status = "paid".to_string();
        invoice.paid_at = Some(now());
    } else {
        invoice.status = "partially_paid".to_string();
    }
    invoice.updated_at = now();
    let fully_paid = invoice.status == "paid";
    let invoice_value = serde_json::to_value(&*invoice).unwrap_or(Value::Null);
    let payment_value = serde_json::to_value(&payment).unwrap_or(Value::Null);
    drop(invoice);
    state.emit("invoice.payment.recorded", payment_value);
    if fully_paid {
        state.emit(
            "invoice.paid",
            json!({ "invoice": invoice_value, "method": method }),
        );
    }
    Ok(payment)
}

/// Mirror one invoice's payment state onto its linked fee charge.
/// Returns the updated charge plus whether it just became paid or
/// just became void, so the event handlers can emit exactly once.
fn sync_fee_charge_from_invoice(
    state: &Arc<BillingState>,
    invoice: &Invoice,
) -> (Option<FeeCharge>, bool, bool) {
    let mut found: Option<FeeCharge> = None;
    for mut entry in state.fee_charges.iter_mut() {
        if entry.value().invoice_id.as_deref() == Some(invoice.id.as_str()) {
            let was_paid = entry.status == "paid";
            let was_void = entry.status == "void";
            entry.amount_paid_minor =
                major_to_minor(invoice.amount_paid, &invoice.currency);
            if invoice.status == "paid" {
                entry.status = "paid".to_string();
                entry.paid_at = invoice.paid_at;
            } else if invoice.status == "void" {
                entry.status = "void".to_string();
            } else if entry.amount_paid_minor > 0 {
                entry.status = "partially_paid".to_string();
            }
            entry.updated_at = now();
            let updated = entry.value().clone();
            let became_paid = !was_paid && updated.status == "paid";
            let became_void = !was_void && updated.status == "void";
            found = Some(updated);
            return (found, became_paid, became_void);
        }
    }
    (None, false, false)
}

/// Print payload for a receipt: the shape the printing module
/// consumes from `school_fees.receipt.print_requested` (and from
/// the read receipt endpoint, without emitting).
fn fee_receipt_print_payload(
    receipt: &FeeReceipt,
    charge: &FeeCharge,
    fee_type_name: &str,
) -> Value {
    json!({
        "document_kind": "school_fee_receipt",
        "receipt_number": receipt.number,
        "student_user_id": receipt.student_user_id,
        "fee_type": fee_type_name,
        "period_label": charge.period_label,
        "academic_period_id": charge.academic_period_id,
        "amount_minor": receipt.amount_minor,
        "amount_text": minor_to_decimal_string(receipt.amount_minor, &receipt.currency),
        "currency": receipt.currency,
        "method": receipt.method,
        "reference": receipt.reference,
        "charge_id": charge.id,
        "invoice_id": charge.invoice_id,
        "due_date": charge.due_date,
        "issued_at": receipt.issued_at,
        "issued_by": receipt.issued_by,
    })
}

// --- School fee hook handlers -----------------------------------------

/// `academic.enrollment.approved`: create schedules for every
/// active fee type marked for auto-assignment, unless a schedule
/// already exists for that student, fee type and academic period.
/// The academic module owns the student, program, level and
/// period; billing only records which fees the enrolment implies.
/// The event carries `{enrollment_id, student_id, subject_id,
/// semester_id}` — no program, level, or period fields, and
/// `student_id` is the academic student record id, not the user
/// id. The student key is read from `student_user_id` (or
/// `user_id`) when an emitter supplies it, falling back to
/// `student_id`; schedules created from the fallback are keyed by
/// the academic student id until academic echoes the user id.
fn on_academic_enrollment_approved(state: Arc<BillingState>, ctx: &mut HookContext) {
    let Some(student_user_id) = ctx
        .get("student_user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("user_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            ctx.get("student_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .filter(|id| !id.trim().is_empty())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "reason": "academic.enrollment.approved carries no student id",
        }));
        return;
    };
    let program_code = ctx
        .get("program_code")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let level_code = ctx
        .get("level_code")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let academic_period_id = ctx
        .get("academic_period_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let period_start = ctx
        .get("period_start")
        .and_then(|value| value.as_str())
        .and_then(|raw| raw.parse::<DateTime<Utc>>().ok());
    let due_in_days = i64::from(state.fee_settings().auto_assign_due_in_days);
    let base = period_start.unwrap_or_else(now);
    let due_date = base + Duration::days(due_in_days);
    let mut created = 0usize;
    let auto_types: Vec<SchoolFeeType> = state
        .fee_types
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|fee_type| fee_type.active && fee_type.auto_assign_on_enrolment)
        .collect();
    for fee_type in auto_types {
        let already_scheduled = state.fee_schedules.iter().any(|entry| {
            let schedule = entry.value();
            schedule.student_user_id == student_user_id
                && schedule.fee_type_id == fee_type.id
                && schedule.academic_period_id == academic_period_id
                && schedule.status == "active"
        });
        if already_scheduled {
            continue;
        }
        let schedule = StudentFeeSchedule {
            id: new_id(),
            student_user_id: student_user_id.clone(),
            program_code: program_code.clone(),
            level_code: level_code.clone(),
            academic_period_id: academic_period_id.clone(),
            fee_type_id: fee_type.id.clone(),
            amount_minor: fee_type.default_amount_minor,
            currency: fee_type.currency.clone(),
            due_date,
            status: "active".to_string(),
            notes: None,
            created_at: now(),
            updated_at: now(),
        };
        state
            .fee_schedules
            .insert(schedule.id.clone(), schedule.clone());
        state.emit(
            "school_fees.schedule.created",
            serde_json::to_value(&schedule).unwrap_or(Value::Null),
        );
        created += 1;
    }
    ctx.add_result(json!({
        "billing": "ok",
        "student_user_id": student_user_id,
        "schedules_created": created,
    }));
}

/// `academic.enrollment.dropped`: cancel the student's active fee
/// schedules and void their open charges (voiding the linked
/// invoice, which the sync handler mirrors onto the charge). The
/// student key follows the same resolution as
/// `on_academic_enrollment_approved` (see its docs).
fn on_academic_enrollment_dropped(state: Arc<BillingState>, ctx: &mut HookContext) {
    let Some(student_user_id) = ctx
        .get("student_user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("user_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            ctx.get("student_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .filter(|id| !id.trim().is_empty())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "reason": "academic.enrollment.dropped carries no student id",
        }));
        return;
    };
    let mut cancelled = 0usize;
    let mut voided = 0usize;
    for mut entry in state.fee_schedules.iter_mut() {
        if entry.student_user_id == student_user_id && entry.status == "active" {
            entry.status = "cancelled".to_string();
            entry.updated_at = now();
            let value = serde_json::to_value(&*entry).unwrap_or(Value::Null);
            drop(entry);
            state.emit("school_fees.schedule.cancelled", value);
            cancelled += 1;
        }
    }
    let open_charge_ids: Vec<String> = state
        .fee_charges
        .iter()
        .filter(|entry| {
            let charge = entry.value();
            charge.student_user_id == student_user_id
                && matches!(charge.status.as_str(), "open" | "partially_paid")
        })
        .map(|entry| entry.key().clone())
        .collect();
    for charge_id in open_charge_ids {
        let Some(charge) = state
            .fee_charges
            .get(&charge_id)
            .map(|entry| entry.value().clone())
        else {
            continue;
        };
        match &charge.invoice_id {
            Some(invoice_id) => {
                if let Some(mut invoice) = state.invoices.get_mut(invoice_id) {
                    if !matches!(invoice.status.as_str(), "paid" | "void" | "credited") {
                        invoice.status = "void".to_string();
                        invoice.updated_at = now();
                        let invoice_value =
                            serde_json::to_value(&*invoice).unwrap_or(Value::Null);
                        drop(invoice);
                        state.emit("invoice.voided", invoice_value);
                        voided += 1;
                    }
                }
            }
            None => {
                if let Some(mut stored) = state.fee_charges.get_mut(&charge_id) {
                    stored.status = "void".to_string();
                    stored.updated_at = now();
                    let value = serde_json::to_value(&*stored).unwrap_or(Value::Null);
                    drop(stored);
                    state.emit("school_fees.charge.voided", value);
                    voided += 1;
                }
            }
        }
    }
    ctx.add_result(json!({
        "billing": "ok",
        "student_user_id": student_user_id,
        "schedules_cancelled": cancelled,
        "charges_voided": voided,
    }));
}

/// `invoice.payment.recorded`: mirror the paid amount onto the
/// linked fee charge (partial payments included).
fn on_invoice_payment_recorded_for_fee(state: Arc<BillingState>, ctx: &mut HookContext) {
    let Some(invoice_id) = ctx
        .get("invoice_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        return;
    };
    let Some(invoice) = state
        .invoices
        .get(&invoice_id)
        .map(|entry| entry.value().clone())
    else {
        return;
    };
    let (updated, _, _) = sync_fee_charge_from_invoice(&state, &invoice);
    if let Some(charge) = updated {
        ctx.add_result(json!({
            "billing": "ok",
            "charge_id": charge.id,
            "amount_paid_minor": charge.amount_paid_minor,
        }));
    }
}

/// `invoice.paid`: mark the linked fee charge paid and announce it
/// for the academic module and the notification flow.
fn on_invoice_paid_for_fee(state: Arc<BillingState>, ctx: &mut HookContext) {
    let Some(invoice_id) = ctx
        .get("invoice")
        .and_then(|value| value.get("id"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        return;
    };
    let Some(invoice) = state
        .invoices
        .get(&invoice_id)
        .map(|entry| entry.value().clone())
    else {
        return;
    };
    let (updated, became_paid, _) = sync_fee_charge_from_invoice(&state, &invoice);
    if let Some(charge) = updated {
        if became_paid {
            state.emit(
                "school_fees.charge.paid",
                serde_json::to_value(&charge).unwrap_or(Value::Null),
            );
        }
        ctx.add_result(json!({
            "billing": "ok",
            "charge_id": charge.id,
            "status": charge.status,
        }));
    }
}

/// `invoice.voided`: mirror the void onto the linked fee charge.
fn on_invoice_voided_for_fee(state: Arc<BillingState>, ctx: &mut HookContext) {
    let Some(invoice_id) = ctx
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        return;
    };
    let Some(invoice) = state
        .invoices
        .get(&invoice_id)
        .map(|entry| entry.value().clone())
    else {
        return;
    };
    let (updated, _, became_void) = sync_fee_charge_from_invoice(&state, &invoice);
    if let Some(charge) = updated {
        if became_void {
            state.emit(
                "school_fees.charge.voided",
                serde_json::to_value(&charge).unwrap_or(Value::Null),
            );
        }
        ctx.add_result(json!({
            "billing": "ok",
            "charge_id": charge.id,
            "status": charge.status,
        }));
    }
}

// --- School fee HTTP handlers -----------------------------------------

// Request bodies. Amounts are integer minor units everywhere.

#[derive(Debug, Clone, Deserialize)]
pub struct CreateFeeTypeRequest {
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub category: String,
    pub billing_period: String,
    pub default_amount_minor: i64,
    pub currency: Option<String>,
    #[serde(default)]
    pub auto_assign_on_enrolment: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateFeeTypeRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub billing_period: Option<String>,
    pub default_amount_minor: Option<i64>,
    pub currency: Option<String>,
    pub auto_assign_on_enrolment: Option<bool>,
    pub active: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateFeeSettingsRequest {
    pub currency: Option<String>,
    pub auto_assign_due_in_days: Option<u32>,
    pub reminder_before_days: Option<Vec<i32>>,
    pub reminder_after_days: Option<Vec<i32>>,
    pub aging_bucket_days: Option<Vec<i32>>,
    pub fee_categories: Option<Vec<String>>,
    pub billing_periods: Option<Vec<String>>,
    pub receipt_prefix: Option<String>,
    pub discount_requires_approval: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateFeeScheduleRequest {
    pub student_user_id: String,
    pub program_code: String,
    pub level_code: String,
    pub academic_period_id: String,
    pub fee_type_id: String,
    pub amount_minor: Option<i64>,
    pub due_date: DateTime<Utc>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateFeeScheduleRequest {
    pub amount_minor: Option<i64>,
    pub due_date: Option<DateTime<Utc>>,
    pub status: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenerateFeeChargeRequest {
    pub period_label: String,
    pub due_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PayFeeChargeRequest {
    pub amount_minor: i64,
    pub method: Option<String>,
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateFeeDiscountRequest {
    pub student_user_id: String,
    pub fee_type_id: Option<String>,
    pub kind: String,
    pub percent_basis_points: Option<i64>,
    pub amount_minor: Option<i64>,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateFeeDiscountRequest {
    pub approved: Option<bool>,
    pub status: Option<String>,
    pub valid_to: Option<DateTime<Utc>>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateFeeGuardianRequest {
    pub guardian_user_id: String,
    pub student_user_id: String,
    pub relationship: Option<String>,
}

fn fee_validate_day_list(
    days: &[i32],
    field: &str,
    max: i32,
) -> UnifiedResult<Vec<i32>> {
    if days.len() > 31 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must have at most 31 entries"
        )));
    }
    for &day in days {
        if day <= 0 || day > max {
            return Err(UnifiedError::bad_request(format!(
                "{field} entries must be between 1 and {max}"
            )));
        }
    }
    let mut sorted = days.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    Ok(sorted)
}

// --- Fee types ---

async fn list_fee_types(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let caller = match fee_caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = fee_require_view(&ctx, &caller) {
        return json::error_response(&error, false);
    }
    let params = query.into_inner();
    let sort = params.allowed_sort(&["created_at", "name", "code"], "created_at");
    let mut items: Vec<SchoolFeeType> = state
        .fee_types
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|fee_type| {
            matches_search(&format!("{} {} {}", fee_type.code, fee_type.name, fee_type.category), &params)
                && params
                    .filters
                    .get("category")
                    .is_none_or(|category| fee_type.category == *category)
                && params
                    .filters
                    .get("billing_period")
                    .is_none_or(|period| fee_type.billing_period == *period)
                && params.filters.get("active").is_none_or(|active| {
                    fee_type.active == (*active == "true")
                })
        })
        .collect();
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        "code" => items.sort_by(|a, b| a.code.cmp(&b.code)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_fee_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    body: web::Json<CreateFeeTypeRequest>,
) -> HttpResponse {
    let result: UnifiedResult<SchoolFeeType> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let input = body.into_inner();
        let settings = state.fee_settings();
        let code = fee_require_text(&input.code, "code", 64)?;
        if state
            .fee_types
            .iter()
            .any(|entry| entry.value().code == code)
        {
            return Err(UnifiedError::conflict(format!(
                "Fee type code \"{code}\" already exists"
            )));
        }
        let name = fee_require_text(&input.name, "name", 200)?;
        let description = fee_optional_text(&input.description, "description", 2000)?;
        let category = fee_require_text(&input.category, "category", 64)?;
        if !settings.fee_categories.iter().any(|item| item == &category) {
            return Err(UnifiedError::bad_request(format!(
                "category \"{category}\" is not allowed; allowed values: {}",
                settings.fee_categories.join(", ")
            )));
        }
        let billing_period = fee_require_text(&input.billing_period, "billing_period", 64)?;
        if !settings.billing_periods.iter().any(|item| item == &billing_period) {
            return Err(UnifiedError::bad_request(format!(
                "billing_period \"{billing_period}\" is not allowed; allowed values: {}",
                settings.billing_periods.join(", ")
            )));
        }
        if input.default_amount_minor < 0 {
            return Err(UnifiedError::bad_request(
                "default_amount_minor must not be negative",
            ));
        }
        let currency = match &input.currency {
            Some(raw) => fee_require_currency(raw)?,
            None => settings.currency.clone(),
        };
        let fee_type = SchoolFeeType {
            id: new_id(),
            code,
            name,
            description,
            category,
            billing_period,
            default_amount_minor: input.default_amount_minor,
            currency,
            auto_assign_on_enrolment: input.auto_assign_on_enrolment,
            active: true,
            created_at: now(),
            updated_at: now(),
        };
        state
            .fee_types
            .insert(fee_type.id.clone(), fee_type.clone());
        state.emit(
            "school_fees.type.created",
            serde_json::to_value(&fee_type).unwrap_or(Value::Null),
        );
        Ok(fee_type)
    })();
    json::respond(result, false)
}

async fn get_fee_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<SchoolFeeType> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_view(&ctx, &caller)?;
        state
            .fee_types
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee type not found"))
    })();
    json::respond(result, false)
}

async fn update_fee_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateFeeTypeRequest>,
) -> HttpResponse {
    let result: UnifiedResult<SchoolFeeType> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let input = body.into_inner();
        let settings = state.fee_settings();
        let mut fee_type = state
            .fee_types
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Fee type not found"))?;
        if let Some(name) = input.name {
            fee_type.name = fee_require_text(&name, "name", 200)?;
        }
        if let Some(description) = input.description {
            fee_type.description = if description.trim().is_empty() {
                None
            } else {
                Some(fee_require_text(&description, "description", 2000)?)
            };
        }
        if let Some(category) = input.category {
            let category = fee_require_text(&category, "category", 64)?;
            if !settings.fee_categories.iter().any(|item| item == &category) {
                return Err(UnifiedError::bad_request(format!(
                    "category \"{category}\" is not allowed; allowed values: {}",
                    settings.fee_categories.join(", ")
                )));
            }
            fee_type.category = category;
        }
        if let Some(billing_period) = input.billing_period {
            let billing_period =
                fee_require_text(&billing_period, "billing_period", 64)?;
            if !settings
                .billing_periods
                .iter()
                .any(|item| item == &billing_period)
            {
                return Err(UnifiedError::bad_request(format!(
                    "billing_period \"{billing_period}\" is not allowed; allowed values: {}",
                    settings.billing_periods.join(", ")
                )));
            }
            fee_type.billing_period = billing_period;
        }
        if let Some(amount) = input.default_amount_minor {
            if amount < 0 {
                return Err(UnifiedError::bad_request(
                    "default_amount_minor must not be negative",
                ));
            }
            fee_type.default_amount_minor = amount;
        }
        if let Some(currency) = input.currency {
            fee_type.currency = fee_require_currency(&currency)?;
        }
        if let Some(auto) = input.auto_assign_on_enrolment {
            fee_type.auto_assign_on_enrolment = auto;
        }
        if let Some(active) = input.active {
            fee_type.active = active;
        }
        fee_type.updated_at = now();
        let updated = fee_type.clone();
        drop(fee_type);
        state.emit(
            "school_fees.type.updated",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_fee_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        if !state.fee_types.contains_key(path.as_str()) {
            return Err(UnifiedError::not_found("Fee type not found"));
        }
        if state.fee_schedules.iter().any(|entry| {
            entry.value().fee_type_id == path.as_str()
        }) {
            return Err(UnifiedError::unprocessable(
                "Fee type is used by fee schedules; deactivate it instead of deleting it",
            ));
        }
        state.fee_types.remove(path.as_str());
        state.emit(
            "school_fees.type.deleted",
            json!({ "fee_type_id": path.as_str() }),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, false)
}

// --- Fee settings ---

async fn get_fee_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
) -> HttpResponse {
    let result: UnifiedResult<SchoolFeeSettings> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_view(&ctx, &caller)?;
        Ok(state.fee_settings())
    })();
    json::respond(result, false)
}

async fn update_fee_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    body: web::Json<UpdateFeeSettingsRequest>,
) -> HttpResponse {
    let result: UnifiedResult<SchoolFeeSettings> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let input = body.into_inner();
        let mut settings = state
            .fee_settings
            .write()
            .expect("school fee settings lock poisoned");
        if let Some(currency) = input.currency {
            settings.currency = fee_require_currency(&currency)?;
        }
        if let Some(days) = input.auto_assign_due_in_days {
            if days > 365 {
                return Err(UnifiedError::bad_request(
                    "auto_assign_due_in_days must be at most 365",
                ));
            }
            settings.auto_assign_due_in_days = days;
        }
        if let Some(days) = input.reminder_before_days {
            settings.reminder_before_days = fee_validate_day_list(&days, "reminder_before_days", 365)?;
        }
        if let Some(days) = input.reminder_after_days {
            settings.reminder_after_days = fee_validate_day_list(&days, "reminder_after_days", 365)?;
        }
        if let Some(days) = input.aging_bucket_days {
            let cutoffs = fee_validate_day_list(&days, "aging_bucket_days", 3650)?;
            if cutoffs.is_empty() {
                return Err(UnifiedError::bad_request(
                    "aging_bucket_days must not be empty",
                ));
            }
            settings.aging_bucket_days = cutoffs;
        }
        if let Some(categories) = input.fee_categories {
            if categories.is_empty() {
                return Err(UnifiedError::bad_request(
                    "fee_categories must not be empty",
                ));
            }
            settings.fee_categories = categories
                .into_iter()
                .map(|category| fee_require_text(&category, "fee_categories", 64))
                .collect::<UnifiedResult<Vec<String>>>()?;
        }
        if let Some(periods) = input.billing_periods {
            if periods.is_empty() {
                return Err(UnifiedError::bad_request(
                    "billing_periods must not be empty",
                ));
            }
            settings.billing_periods = periods
                .into_iter()
                .map(|period| fee_require_text(&period, "billing_periods", 64))
                .collect::<UnifiedResult<Vec<String>>>()?;
        }
        if let Some(prefix) = input.receipt_prefix {
            settings.receipt_prefix = fee_require_text(&prefix, "receipt_prefix", 16)?;
        }
        if let Some(requires) = input.discount_requires_approval {
            settings.discount_requires_approval = requires;
        }
        let updated = settings.clone();
        drop(settings);
        state.emit(
            "school_fees.settings.updated",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

// --- Fee schedules ---

async fn create_fee_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    body: web::Json<CreateFeeScheduleRequest>,
) -> HttpResponse {
    let result: UnifiedResult<StudentFeeSchedule> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let input = body.into_inner();
        let student_user_id = fee_require_text(&input.student_user_id, "student_user_id", 128)?;
        let program_code = fee_require_text(&input.program_code, "program_code", 128)?;
        let level_code = fee_require_text(&input.level_code, "level_code", 128)?;
        let academic_period_id =
            fee_require_text(&input.academic_period_id, "academic_period_id", 128)?;
        let fee_type = state
            .fee_types
            .get(&input.fee_type_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee type not found"))?;
        if !fee_type.active {
            return Err(UnifiedError::unprocessable(
                "Fee type is not active",
            ));
        }
        let amount_minor = match input.amount_minor {
            Some(amount) => {
                if amount < 0 {
                    return Err(UnifiedError::bad_request(
                        "amount_minor must not be negative",
                    ));
                }
                amount
            }
            None => fee_type.default_amount_minor,
        };
        let notes = fee_optional_text(&input.notes, "notes", 2000)?;
        let schedule = StudentFeeSchedule {
            id: new_id(),
            student_user_id,
            program_code,
            level_code,
            academic_period_id,
            fee_type_id: fee_type.id,
            amount_minor,
            currency: fee_type.currency,
            due_date: input.due_date,
            status: "active".to_string(),
            notes,
            created_at: now(),
            updated_at: now(),
        };
        state
            .fee_schedules
            .insert(schedule.id.clone(), schedule.clone());
        state.emit(
            "school_fees.schedule.created",
            serde_json::to_value(&schedule).unwrap_or(Value::Null),
        );
        Ok(schedule)
    })();
    json::respond(result, false)
}

async fn list_fee_schedules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let caller = match fee_caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = fee_require_view(&ctx, &caller) {
        return json::error_response(&error, false);
    }
    let is_manager = fee_is_manager(&ctx, &caller);
    let visible = fee_visible_student_ids(&state, &caller);
    let params = query.into_inner();
    let sort = params.allowed_sort(&["created_at", "due_date"], "created_at");
    let mut items: Vec<StudentFeeSchedule> = state
        .fee_schedules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|schedule| {
            (is_manager || visible.iter().any(|id| id == &schedule.student_user_id))
                && params
                    .filters
                    .get("student_user_id")
                    .is_none_or(|student| schedule.student_user_id == *student)
                && params
                    .filters
                    .get("fee_type_id")
                    .is_none_or(|fee_type| schedule.fee_type_id == *fee_type)
                && params
                    .filters
                    .get("academic_period_id")
                    .is_none_or(|period| schedule.academic_period_id == *period)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| schedule.status == *status)
        })
        .collect();
    match sort.as_str() {
        "due_date" => items.sort_by(|a, b| a.due_date.cmp(&b.due_date)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn get_fee_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<StudentFeeSchedule> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_view(&ctx, &caller)?;
        let schedule = state
            .fee_schedules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee schedule not found"))?;
        if !fee_can_view_student(&ctx, &state, &caller, &schedule.student_user_id) {
            return Err(UnifiedError::forbidden(
                "Fee schedule belongs to a different student",
            ));
        }
        Ok(schedule)
    })();
    json::respond(result, false)
}

async fn update_fee_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateFeeScheduleRequest>,
) -> HttpResponse {
    let result: UnifiedResult<StudentFeeSchedule> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let input = body.into_inner();
        let mut schedule = state
            .fee_schedules
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Fee schedule not found"))?;
        if let Some(amount) = input.amount_minor {
            if amount < 0 {
                return Err(UnifiedError::bad_request(
                    "amount_minor must not be negative",
                ));
            }
            schedule.amount_minor = amount;
        }
        if let Some(due_date) = input.due_date {
            schedule.due_date = due_date;
        }
        if let Some(status) = input.status {
            if !matches!(status.as_str(), "active" | "paused" | "cancelled") {
                return Err(UnifiedError::bad_request(
                    "status must be active, paused or cancelled",
                ));
            }
            schedule.status = status;
        }
        if let Some(notes) = input.notes {
            schedule.notes = if notes.trim().is_empty() {
                None
            } else {
                Some(fee_require_text(&notes, "notes", 2000)?)
            };
        }
        schedule.updated_at = now();
        let updated = schedule.clone();
        drop(schedule);
        state.emit(
            "school_fees.schedule.updated",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

/// Generate one fee charge for a schedule and period, with its
/// linked billing invoice. Approved, in-period discounts for the
/// student apply automatically. A fully discounted charge needs no
/// invoice and is recorded as paid.
async fn generate_fee_charges(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<GenerateFeeChargeRequest>,
) -> HttpResponse {
    let result: UnifiedResult<FeeCharge> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let input = body.into_inner();
        let period_label = fee_require_text(&input.period_label, "period_label", 32)?;
        let schedule = state
            .fee_schedules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee schedule not found"))?;
        if schedule.status != "active" {
            return Err(UnifiedError::unprocessable(format!(
                "Cannot generate charges for a {} schedule",
                schedule.status
            )));
        }
        let fee_type = state
            .fee_types
            .get(&schedule.fee_type_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee type not found"))?;
        if state.fee_charges.iter().any(|entry| {
            let charge = entry.value();
            charge.schedule_id == schedule.id
                && charge.period_label == period_label
                && charge.status != "void"
        }) {
            return Err(UnifiedError::conflict(format!(
                "A charge already exists for period \"{period_label}\" on this schedule"
            )));
        }
        let at = now();
        let discounts = fee_applicable_discounts(
            &state,
            &schedule.student_user_id,
            &schedule.fee_type_id,
            at,
        );
        let gross = schedule.amount_minor;
        let mut discount_minor = 0i64;
        let mut discount_ids: Vec<String> = Vec::new();
        for discount in &discounts {
            discount_minor += fee_discount_amount(gross, discount);
            discount_ids.push(discount.id.clone());
        }
        let discount_minor = discount_minor.min(gross);
        let total_minor = gross - discount_minor;
        let due_date = input.due_date.unwrap_or(schedule.due_date);
        let mut charge = FeeCharge {
            id: new_id(),
            schedule_id: schedule.id.clone(),
            student_user_id: schedule.student_user_id.clone(),
            fee_type_id: schedule.fee_type_id.clone(),
            academic_period_id: schedule.academic_period_id.clone(),
            period_label: period_label.clone(),
            amount_minor: gross,
            discount_minor,
            total_minor,
            amount_paid_minor: 0,
            currency: schedule.currency.clone(),
            invoice_id: None,
            discount_ids,
            due_date,
            status: "open".to_string(),
            overdue: false,
            paid_at: None,
            created_at: at,
            updated_at: at,
        };
        if total_minor == 0 {
            // Fully discounted: nothing to collect.
            charge.status = "paid".to_string();
            charge.paid_at = Some(at);
            state
                .fee_charges
                .insert(charge.id.clone(), charge.clone());
            state.emit(
                "school_fees.charge.created",
                serde_json::to_value(&charge).unwrap_or(Value::Null),
            );
            state.emit(
                "school_fees.charge.paid",
                serde_json::to_value(&charge).unwrap_or(Value::Null),
            );
            return Ok(view_fee_charge(&charge));
        }
        let invoice = build_fee_invoice(&state, &charge, &fee_type, &period_label);
        charge.invoice_id = Some(invoice.id.clone());
        state
            .fee_charges
            .insert(charge.id.clone(), charge.clone());
        state.emit(
            "school_fees.charge.created",
            serde_json::to_value(&charge).unwrap_or(Value::Null),
        );
        Ok(view_fee_charge(&charge))
    })();
    json::respond(result, false)
}

// --- Fee charges ---

async fn list_fee_charges(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let caller = match fee_caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = fee_require_view(&ctx, &caller) {
        return json::error_response(&error, false);
    }
    let is_manager = fee_is_manager(&ctx, &caller);
    let visible = fee_visible_student_ids(&state, &caller);
    let params = query.into_inner();
    let sort = params.allowed_sort(&["created_at", "due_date", "total_minor"], "created_at");
    let at = now();
    let mut items: Vec<FeeCharge> = state
        .fee_charges
        .iter()
        .map(|entry| view_fee_charge(entry.value()))
        .filter(|charge| {
            (is_manager || visible.iter().any(|id| id == &charge.student_user_id))
                && matches_search(
                    &format!("{} {} {}", charge.period_label, charge.id, charge.student_user_id),
                    &params,
                )
                && params
                    .filters
                    .get("student_user_id")
                    .is_none_or(|student| charge.student_user_id == *student)
                && params
                    .filters
                    .get("fee_type_id")
                    .is_none_or(|fee_type| charge.fee_type_id == *fee_type)
                && params
                    .filters
                    .get("academic_period_id")
                    .is_none_or(|period| charge.academic_period_id == *period)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| fee_charge_effective_status(charge, at) == *status)
        })
        .collect();
    match sort.as_str() {
        "due_date" => items.sort_by(|a, b| a.due_date.cmp(&b.due_date)),
        "total_minor" => items.sort_by(|a, b| a.total_minor.cmp(&b.total_minor)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn get_fee_charge(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<FeeCharge> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_view(&ctx, &caller)?;
        let charge = state
            .fee_charges
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee charge not found"))?;
        fee_require_charge_access(&ctx, &state, &caller, &charge)?;
        Ok(view_fee_charge(&charge))
    })();
    json::respond(result, false)
}

/// Pay a fee charge. The payment is recorded against the linked
/// billing invoice through the shared invoice payment path, so
/// gateway captures, manual invoice payments and fee payments all
/// land in the same payment history.
async fn pay_fee_charge(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PayFeeChargeRequest>,
) -> HttpResponse {
    let result: UnifiedResult<FeeCharge> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_view(&ctx, &caller)?;
        let input = body.into_inner();
        if input.amount_minor <= 0 {
            return Err(UnifiedError::bad_request(
                "amount_minor must be a positive number",
            ));
        }
        let method = input
            .method
            .unwrap_or_else(|| "manual".to_string());
        if method.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Payment method must not be empty",
            ));
        }
        let reference = input
            .reference
            .filter(|reference| !reference.trim().is_empty())
            .map(|reference| {
                if reference.len() > 128 {
                    reference.chars().take(128).collect()
                } else {
                    reference
                }
            });
        let charge = state
            .fee_charges
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee charge not found"))?;
        // Managers may pay any charge; anyone else may only pay a
        // charge they can view (their own, or their child's).
        if !fee_is_manager(&ctx, &caller) {
            fee_require_charge_access(&ctx, &state, &caller, &charge)?;
        }
        if charge.status == "paid" {
            return Err(UnifiedError::conflict("Fee charge is already paid"));
        }
        if charge.status == "void" {
            return Err(UnifiedError::unprocessable(
                "A void fee charge cannot be paid",
            ));
        }
        let remaining = charge.total_minor - charge.amount_paid_minor;
        if input.amount_minor > remaining {
            return Err(UnifiedError::unprocessable(format!(
                "Payment of {} exceeds the outstanding balance of {}",
                minor_to_decimal_string(input.amount_minor, &charge.currency),
                minor_to_decimal_string(remaining, &charge.currency),
            )));
        }
        record_fee_invoice_payment(&state, &charge, input.amount_minor, &method, reference)?;
        // The invoice-event sync handlers updated the charge above;
        // re-read it for the response.
        state
            .fee_charges
            .get(path.as_str())
            .map(|entry| view_fee_charge(entry.value()))
            .ok_or_else(|| UnifiedError::not_found("Fee charge not found"))
    })();
    json::respond(result, false)
}

async fn void_fee_charge(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<FeeCharge> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let charge = state
            .fee_charges
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee charge not found"))?;
        if charge.status == "paid" {
            return Err(UnifiedError::unprocessable(
                "A paid fee charge cannot be voided",
            ));
        }
        if charge.status == "void" {
            return Err(UnifiedError::conflict("Fee charge is already void"));
        }
        match &charge.invoice_id {
            Some(invoice_id) => {
                let mut invoice = state
                    .invoices
                    .get_mut(invoice_id)
                    .ok_or_else(|| UnifiedError::not_found("Linked invoice not found"))?;
                invoice.status = "void".to_string();
                invoice.updated_at = now();
                let invoice_value = serde_json::to_value(&*invoice).unwrap_or(Value::Null);
                drop(invoice);
                // The invoice.voided sync handler mirrors the void
                // onto the charge and emits school_fees.charge.voided.
                state.emit("invoice.voided", invoice_value);
            }
            None => {
                if let Some(mut stored) = state.fee_charges.get_mut(path.as_str()) {
                    stored.status = "void".to_string();
                    stored.updated_at = now();
                    let value = serde_json::to_value(&*stored).unwrap_or(Value::Null);
                    drop(stored);
                    state.emit("school_fees.charge.voided", value);
                }
            }
        }
        state
            .fee_charges
            .get(path.as_str())
            .map(|entry| view_fee_charge(entry.value()))
            .ok_or_else(|| UnifiedError::not_found("Fee charge not found"))
    })();
    json::respond(result, false)
}

// --- Fee receipts ---

async fn get_fee_receipt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_view(&ctx, &caller)?;
        let charge = state
            .fee_charges
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee charge not found"))?;
        fee_require_charge_access(&ctx, &state, &caller, &charge)?;
        let receipt = state
            .fee_receipts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|receipt| receipt.charge_id == charge.id)
            .max_by_key(|receipt| receipt.issued_at)
            .ok_or_else(|| {
                UnifiedError::not_found("No receipt issued for this charge yet")
            })?;
        let fee_type_name = state
            .fee_types
            .get(&charge.fee_type_id)
            .map(|entry| entry.value().name.clone())
            .unwrap_or_default();
        Ok(json!({
            "receipt": receipt,
            "charge": view_fee_charge(&charge),
            "print": fee_receipt_print_payload(&receipt, &charge, &fee_type_name),
        }))
    })();
    json::respond(result, false)
}

/// Issue a receipt for the amount paid so far and request
/// printing: the receipt is data, and the print request goes out
/// as `school_fees.receipt.print_requested` for the printing
/// module. This endpoint never renders or prints anything itself.
async fn issue_fee_receipt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let charge = state
            .fee_charges
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee charge not found"))?;
        if charge.amount_paid_minor <= 0 {
            return Err(UnifiedError::unprocessable(
                "Cannot issue a receipt for a charge with no payment recorded",
            ));
        }
        let fee_type_name = state
            .fee_types
            .get(&charge.fee_type_id)
            .map(|entry| entry.value().name.clone())
            .unwrap_or_default();
        let receipt = FeeReceipt {
            id: new_id(),
            number: state.next_fee_receipt_number(),
            charge_id: charge.id.clone(),
            student_user_id: charge.student_user_id.clone(),
            amount_minor: charge.amount_paid_minor,
            currency: charge.currency.clone(),
            method: None,
            reference: charge.invoice_id.clone(),
            issued_by: caller.actor.clone(),
            issued_at: now(),
        };
        state
            .fee_receipts
            .insert(receipt.id.clone(), receipt.clone());
        let payload = fee_receipt_print_payload(&receipt, &charge, &fee_type_name);
        state.emit(
            "school_fees.receipt.issued",
            serde_json::to_value(&receipt).unwrap_or(Value::Null),
        );
        state.emit("school_fees.receipt.print_requested", payload.clone());
        Ok(json!({
            "receipt": receipt,
            "charge": view_fee_charge(&charge),
            "print": payload,
        }))
    })();
    json::respond(result, false)
}

// --- Fee reminders ---

/// Send one manual reminder for a charge to the student and their
/// linked guardians.
async fn remind_fee_charge(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let charge = state
            .fee_charges
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee charge not found"))?;
        if charge.total_minor - charge.amount_paid_minor <= 0
            || matches!(charge.status.as_str(), "paid" | "void")
        {
            return Err(UnifiedError::unprocessable(
                "Fee charge has no outstanding balance to remind about",
            ));
        }
        let fee_type_name = state
            .fee_types
            .get(&charge.fee_type_id)
            .map(|entry| entry.value().name.clone())
            .unwrap_or_default();
        let remaining = charge.total_minor - charge.amount_paid_minor;
        let title = format!("Fee reminder: {fee_type_name} ({})", charge.period_label);
        let body = format!(
            "An amount of {} {} for {} ({}) is due on {}. Outstanding: {} {}.",
            minor_to_decimal_string(charge.total_minor, &charge.currency),
            charge.currency,
            fee_type_name,
            charge.period_label,
            charge.due_date.format("%Y-%m-%d"),
            minor_to_decimal_string(remaining, &charge.currency),
            charge.currency,
        );
        let mut sent = 0usize;
        for recipient in fee_reminder_recipients(&state, &charge) {
            let key = format!("{}:0:{}", charge.id, recipient);
            if state.fee_reminders.contains_key(&key) {
                continue;
            }
            state.request_notification(&recipient, &title, &body, "school_fee_reminder");
            state.fee_reminders.insert(
                key,
                FeeReminder {
                    charge_id: charge.id.clone(),
                    offset_days: 0,
                    recipient_user_id: recipient.clone(),
                    sent_at: now(),
                },
            );
            state.emit(
                "school_fees.fee.reminder",
                json!({
                    "charge_id": charge.id,
                    "student_user_id": charge.student_user_id,
                    "recipient_user_id": recipient,
                    "offset_days": 0,
                }),
            );
            sent += 1;
        }
        Ok(json!({ "reminders_sent": sent, "charge_id": charge.id }))
    })();
    json::respond(result, false)
}

/// Run the fee-reminder scan: for every open charge, send the
/// reminders that fall due today (before and after the due date,
/// per the settings) to the student and their linked guardians.
/// Each reminder is sent once per recipient.
async fn run_fee_reminders(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let settings = state.fee_settings();
        let at = now();
        let charges: Vec<FeeCharge> = state
            .fee_charges
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        let mut sent = 0usize;
        let mut charges_touched = 0usize;
        let mut newly_overdue = 0usize;
        for charge in &charges {
            let due_offsets = fee_reminders_due(charge, &settings, at);
            if due_offsets.is_empty() {
                continue;
            }
            let fee_type_name = state
                .fee_types
                .get(&charge.fee_type_id)
                .map(|entry| entry.value().name.clone())
                .unwrap_or_default();
            let remaining = charge.total_minor - charge.amount_paid_minor;
            let mut touched = false;
            for offset in due_offsets {
                let title = format!(
                    "Fee reminder: {fee_type_name} ({})",
                    charge.period_label
                );
                let body = if offset > 0 {
                    format!(
                        "An amount of {} {} for {} ({}) is due in {offset} day(s) on {}. Outstanding: {} {}.",
                        minor_to_decimal_string(charge.total_minor, &charge.currency),
                        charge.currency,
                        fee_type_name,
                        charge.period_label,
                        charge.due_date.format("%Y-%m-%d"),
                        minor_to_decimal_string(remaining, &charge.currency),
                        charge.currency,
                    )
                } else {
                    format!(
                        "An amount of {} {} for {} ({}) was due on {} and is {} day(s) overdue. Outstanding: {} {}.",
                        minor_to_decimal_string(charge.total_minor, &charge.currency),
                        charge.currency,
                        fee_type_name,
                        charge.period_label,
                        charge.due_date.format("%Y-%m-%d"),
                        offset.abs(),
                        minor_to_decimal_string(remaining, &charge.currency),
                        charge.currency,
                    )
                };
                for recipient in fee_reminder_recipients(&state, charge) {
                    let key = format!("{}:{offset}:{}", charge.id, recipient);
                    if state.fee_reminders.contains_key(&key) {
                        continue;
                    }
                    state.request_notification(
                        &recipient,
                        &title,
                        &body,
                        "school_fee_reminder",
                    );
                    state.fee_reminders.insert(
                        key,
                        FeeReminder {
                            charge_id: charge.id.clone(),
                            offset_days: offset,
                            recipient_user_id: recipient.clone(),
                            sent_at: at,
                        },
                    );
                    state.emit(
                        "school_fees.fee.reminder",
                        json!({
                            "charge_id": charge.id,
                            "student_user_id": charge.student_user_id,
                            "recipient_user_id": recipient,
                            "offset_days": offset,
                        }),
                    );
                    sent += 1;
                    touched = true;
                }
            }
            if touched {
                charges_touched += 1;
            }
            if fee_charge_is_overdue(charge, at) {
                newly_overdue += 1;
            }
        }
        state.emit(
            "school_fees.reminders.completed",
            json!({
                "reminders_sent": sent,
                "charges": charges_touched,
                "overdue_charges": newly_overdue,
            }),
        );
        Ok(json!({
            "reminders_sent": sent,
            "charges": charges_touched,
            "overdue_charges": newly_overdue,
        }))
    })();
    json::respond(result, false)
}

// --- Arrears ---

async fn list_fee_arrears(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let caller = match fee_caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = fee_require_view(&ctx, &caller) {
        return json::error_response(&error, false);
    }
    let is_manager = fee_is_manager(&ctx, &caller);
    let visible = fee_visible_student_ids(&state, &caller);
    let params = query.into_inner();
    let settings = state.fee_settings();
    let at = now();
    let mut by_student: std::collections::BTreeMap<String, StudentArrears> =
        std::collections::BTreeMap::new();
    for entry in state.fee_charges.iter() {
        let charge = entry.value();
        if !fee_charge_is_overdue(charge, at) {
            continue;
        }
        if !is_manager && !visible.iter().any(|id| id == &charge.student_user_id) {
            continue;
        }
        if let Some(student) = params.filters.get("student_user_id") {
            if charge.student_user_id != *student {
                continue;
            }
        }
        if let Some(period) = params.filters.get("academic_period_id") {
            if charge.academic_period_id != *period {
                continue;
            }
        }
        let overdue_minor = charge.total_minor - charge.amount_paid_minor;
        let days = fee_days_overdue(charge, at);
        let bucket = fee_aging_bucket(&settings.aging_bucket_days, days);
        let summary = by_student
            .entry(charge.student_user_id.clone())
            .or_insert_with(|| StudentArrears {
                student_user_id: charge.student_user_id.clone(),
                total_overdue_minor: 0,
                currency: charge.currency.clone(),
                charge_count: 0,
                oldest_due_date: None,
                buckets: Vec::new(),
            });
        summary.total_overdue_minor += overdue_minor;
        summary.charge_count += 1;
        if summary
            .oldest_due_date
            .is_none_or(|oldest| charge.due_date < oldest)
        {
            summary.oldest_due_date = Some(charge.due_date);
        }
        match summary.buckets.iter_mut().find(|item| item.bucket == bucket) {
            Some(item) => {
                item.amount_minor += overdue_minor;
                item.charge_count += 1;
            }
            None => summary.buckets.push(ArrearsBucket {
                bucket,
                amount_minor: overdue_minor,
                charge_count: 1,
            }),
        }
    }
    let mut items: Vec<StudentArrears> = by_student.into_values().collect();
    items.sort_by(|a, b| b.total_overdue_minor.cmp(&a.total_overdue_minor));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

// --- Fee discounts ---

async fn list_fee_discounts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let caller = match fee_caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = fee_require_view(&ctx, &caller) {
        return json::error_response(&error, false);
    }
    let is_manager = fee_is_manager(&ctx, &caller);
    let visible = fee_visible_student_ids(&state, &caller);
    let params = query.into_inner();
    let sort = params.allowed_sort(&["created_at"], "created_at");
    let mut items: Vec<FeeDiscount> = state
        .fee_discounts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|discount| {
            (is_manager || visible.iter().any(|id| id == &discount.student_user_id))
                && params
                    .filters
                    .get("student_user_id")
                    .is_none_or(|student| discount.student_user_id == *student)
                && params
                    .filters
                    .get("fee_type_id")
                    .is_none_or(|fee_type| {
                        discount.fee_type_id.as_deref() == Some(fee_type.as_str())
                    })
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| discount.status == *status)
                && params.filters.get("approved").is_none_or(|approved| {
                    discount.approved == (*approved == "true")
                })
        })
        .collect();
    match sort.as_str() {
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_fee_discount(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    body: web::Json<CreateFeeDiscountRequest>,
) -> HttpResponse {
    let result: UnifiedResult<FeeDiscount> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let input = body.into_inner();
        let settings = state.fee_settings();
        let student_user_id = fee_require_text(&input.student_user_id, "student_user_id", 128)?;
        let fee_type_id = match &input.fee_type_id {
            Some(raw) => {
                let id = fee_require_text(raw, "fee_type_id", 128)?;
                if !state.fee_types.contains_key(id.as_str()) {
                    return Err(UnifiedError::not_found("Fee type not found"));
                }
                Some(id)
            }
            None => None,
        };
        let kind = fee_require_text(&input.kind, "kind", 32)?;
        if !matches!(kind.as_str(), "percentage" | "fixed") {
            return Err(UnifiedError::bad_request(
                "kind must be percentage or fixed",
            ));
        }
        let (percent_basis_points, amount_minor) = if kind == "percentage" {
            let basis = input.percent_basis_points.ok_or_else(|| {
                UnifiedError::bad_request(
                    "percent_basis_points is required for a percentage discount",
                )
            })?;
            if basis <= 0 || basis > 10_000 {
                return Err(UnifiedError::bad_request(
                    "percent_basis_points must be between 1 and 10000",
                ));
            }
            (basis, 0)
        } else {
            let amount = input.amount_minor.ok_or_else(|| {
                UnifiedError::bad_request("amount_minor is required for a fixed discount")
            })?;
            if amount <= 0 {
                return Err(UnifiedError::bad_request(
                    "amount_minor must be positive",
                ));
            }
            (0, amount)
        };
        if let (Some(from), Some(to)) = (input.valid_from, input.valid_to) {
            if to < from {
                return Err(UnifiedError::bad_request(
                    "valid_to must not be before valid_from",
                ));
            }
        }
        let reason = fee_optional_text(&input.reason, "reason", 2000)?;
        // When approval is configured, a new discount starts
        // unapproved; a manager approves it with an update.
        let approved = !settings.discount_requires_approval;
        let discount = FeeDiscount {
            id: new_id(),
            student_user_id,
            fee_type_id,
            kind,
            percent_basis_points,
            amount_minor,
            valid_from: input.valid_from,
            valid_to: input.valid_to,
            approved,
            approved_by: None,
            reason,
            status: "active".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state
            .fee_discounts
            .insert(discount.id.clone(), discount.clone());
        state.emit(
            "school_fees.discount.created",
            serde_json::to_value(&discount).unwrap_or(Value::Null),
        );
        Ok(discount)
    })();
    json::respond(result, false)
}

async fn get_fee_discount(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<FeeDiscount> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_view(&ctx, &caller)?;
        let discount = state
            .fee_discounts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee discount not found"))?;
        if !fee_can_view_student(&ctx, &state, &caller, &discount.student_user_id) {
            return Err(UnifiedError::forbidden(
                "Fee discount belongs to a different student",
            ));
        }
        Ok(discount)
    })();
    json::respond(result, false)
}

async fn update_fee_discount(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateFeeDiscountRequest>,
) -> HttpResponse {
    let result: UnifiedResult<FeeDiscount> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let input = body.into_inner();
        let mut discount = state
            .fee_discounts
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Fee discount not found"))?;
        let was_approved = discount.approved;
        if let Some(approved) = input.approved {
            discount.approved = approved;
            if approved {
                discount.approved_by = Some(caller.actor.clone());
            } else {
                discount.approved_by = None;
            }
        }
        if let Some(status) = input.status {
            if !matches!(status.as_str(), "active" | "revoked") {
                return Err(UnifiedError::bad_request(
                    "status must be active or revoked",
                ));
            }
            discount.status = status;
        }
        if let Some(valid_to) = input.valid_to {
            if let Some(from) = discount.valid_from {
                if valid_to < from {
                    return Err(UnifiedError::bad_request(
                        "valid_to must not be before valid_from",
                    ));
                }
            }
            discount.valid_to = Some(valid_to);
        }
        if let Some(reason) = input.reason {
            discount.reason = if reason.trim().is_empty() {
                None
            } else {
                Some(fee_require_text(&reason, "reason", 2000)?)
            };
        }
        discount.updated_at = now();
        let updated = discount.clone();
        let newly_approved = !was_approved && updated.approved;
        drop(discount);
        if newly_approved {
            state.emit(
                "school_fees.discount.approved",
                serde_json::to_value(&updated).unwrap_or(Value::Null),
            );
        }
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_fee_discount(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        if !state.fee_discounts.contains_key(path.as_str()) {
            return Err(UnifiedError::not_found("Fee discount not found"));
        }
        state.fee_discounts.remove(path.as_str());
        state.emit(
            "school_fees.discount.deleted",
            json!({ "discount_id": path.as_str() }),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, false)
}

// --- Fee guardians ---

async fn list_fee_guardians(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let caller = match fee_caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = fee_require_view(&ctx, &caller) {
        return json::error_response(&error, false);
    }
    let is_manager = fee_is_manager(&ctx, &caller);
    let params = query.into_inner();
    let mut items: Vec<FeeGuardianLink> = state
        .fee_guardians
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|link| {
            (is_manager
                || link.guardian_user_id == caller.actor
                || fee_visible_student_ids(&state, &caller)
                    .iter()
                    .any(|id| id == &link.student_user_id))
                && params
                    .filters
                    .get("student_user_id")
                    .is_none_or(|student| link.student_user_id == *student)
                && params
                    .filters
                    .get("guardian_user_id")
                    .is_none_or(|guardian| link.guardian_user_id == *guardian)
                && params.filters.get("active").is_none_or(|active| {
                    link.active == (*active == "true")
                })
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_fee_guardian(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    body: web::Json<CreateFeeGuardianRequest>,
) -> HttpResponse {
    let result: UnifiedResult<FeeGuardianLink> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let input = body.into_inner();
        let guardian_user_id =
            fee_require_text(&input.guardian_user_id, "guardian_user_id", 128)?;
        let student_user_id =
            fee_require_text(&input.student_user_id, "student_user_id", 128)?;
        if guardian_user_id == student_user_id {
            return Err(UnifiedError::bad_request(
                "guardian_user_id must differ from student_user_id",
            ));
        }
        let relationship = input
            .relationship
            .map(|raw| fee_require_text(&raw, "relationship", 64))
            .transpose()?
            .unwrap_or_else(|| "guardian".to_string());
        if state.fee_guardians.iter().any(|entry| {
            let link = entry.value();
            link.active
                && link.guardian_user_id == guardian_user_id
                && link.student_user_id == student_user_id
        }) {
            return Err(UnifiedError::conflict(
                "Guardian is already linked to this student",
            ));
        }
        let link = FeeGuardianLink {
            id: new_id(),
            guardian_user_id,
            student_user_id,
            relationship,
            active: true,
            created_at: now(),
        };
        state
            .fee_guardians
            .insert(link.id.clone(), link.clone());
        state.emit(
            "school_fees.guardian.linked",
            serde_json::to_value(&link).unwrap_or(Value::Null),
        );
        Ok(link)
    })();
    json::respond(result, false)
}

async fn delete_fee_guardian(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let mut link = state
            .fee_guardians
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Guardian link not found"))?;
        if !link.active {
            return Err(UnifiedError::conflict("Guardian link is already inactive"));
        }
        link.active = false;
        let updated = link.clone();
        drop(link);
        state.emit(
            "school_fees.guardian.unlinked",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, false)
}

// --- Self-service ---

/// Fee charges for the caller: the student's own charges, or their
/// linked children's charges for a guardian.
async fn list_my_fee_charges(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let caller = match fee_caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = fee_require_view(&ctx, &caller) {
        return json::error_response(&error, false);
    }
    let visible = fee_visible_student_ids(&state, &caller);
    let params = query.into_inner();
    let sort = params.allowed_sort(&["created_at", "due_date", "total_minor"], "created_at");
    let at = now();
    let mut items: Vec<FeeCharge> = state
        .fee_charges
        .iter()
        .map(|entry| view_fee_charge(entry.value()))
        .filter(|charge| {
            visible.iter().any(|id| id == &charge.student_user_id)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| fee_charge_effective_status(charge, at) == *status)
                && params
                    .filters
                    .get("academic_period_id")
                    .is_none_or(|period| charge.academic_period_id == *period)
        })
        .collect();
    match sort.as_str() {
        "due_date" => items.sort_by(|a, b| a.due_date.cmp(&b.due_date)),
        "total_minor" => items.sort_by(|a, b| a.total_minor.cmp(&b.total_minor)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

// ---------------------------------------------------------------------------
// Invoice templates and printing
// ---------------------------------------------------------------------------
//
// Invoice templates are data, never hard-coded layouts: an admin
// (or a company, for its own templates) edits the header/footer,
// logo reference, colours, columns, line-item layout, tax
// breakdown flag, payment instructions, and footer notes through
// the API, and every edit archives the previous version before
// bumping the version number, so any version can be restored as a
// new version. Printing an invoice renders a print-ready HTML
// document with `@page` rules for the chosen paper size from the
// resolved template and queues the job through the printing module
// over the `printing.job.requested` event (the printing module
// owns the queue, the paper-size registry, and the ESC/POS stream);
// billing then emits `billing.invoice.printed`. Emailing an invoice
// sends the same document through `mail.send_requested` — the mail
// module has no attachment field, so the document travels as the
// HTML body, and binary PDF conversion stays a deployment step
// (headless browser or a configured converter hook).

/// Read invoice templates.
const PERM_BILLING_TEMPLATE_VIEW: &str = "billing.template.view";
/// Create, update, delete, restore, and set-default on invoice templates.
const PERM_BILLING_TEMPLATE_MANAGE: &str = "billing.template.manage";
/// Print invoices, list their print jobs, reprint, and email them.
const PERM_BILLING_PRINT: &str = "billing.print";

/// Event emitted when an invoice is printed. Payload:
/// `{"invoice_id", "invoice_number", "job_id", "template_id",
/// "template_version", "output_kind", "requested_by"}`.
pub const EVENT_INVOICE_PRINTED: &str = "billing.invoice.printed";
/// Event emitted when an invoice is emailed. Payload:
/// `{"invoice_id", "invoice_number", "to", "mail_job_id",
/// "requested_by"}`.
pub const EVENT_INVOICE_EMAILED: &str = "billing.invoice.emailed";
/// Event emitted on every invoice-template change (create, update,
/// delete, restore, set-default), for the audit log. Payload:
/// `{"template_id", "company_id", "version", "action",
/// "updated_by"}`.
pub const EVENT_INVOICE_TEMPLATE_CHANGED: &str = "billing.invoice_template.changed";

/// Column keys an invoice template may declare. The list is the
/// closed vocabulary the renderer understands; a new column key
/// needs a renderer case, never a migration.
pub const TEMPLATE_COLUMNS: &[&str] = &[
    "item",
    "quantity",
    "unit_price",
    "tax_amount",
    "line_total",
    "tax_rate",
];

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the assets and POS modules.
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

/// One line-item column of an invoice template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateColumn {
    pub key: String,
    pub label: String,
    pub visible: bool,
}

/// Input for one template column. `visible` defaults to true.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateColumnInput {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub visible: Option<bool>,
}

/// One invoice template: the full layout of a printed invoice, as
/// data. `paper_size_code` names a size from the printing module's
/// registry (for example `a4` or `thermal-80`); billing validates
/// only its shape, the printing module validates its existence at
/// print time. `logo_storage_target` + `logo_storage_key` reference
/// logo bytes in the storage module; `logo_url` is an optional
/// directly usable URL. `line_item_layout` is a free-form JSON
/// object of layout options (row density and similar) so a new
/// option never needs a migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvoiceTemplate {
    pub id: String,
    #[serde(default)]
    pub company_id: Option<String>,
    pub name: String,
    pub is_default: bool,
    pub paper_size_code: String,
    #[serde(default)]
    pub header_text: Option<String>,
    #[serde(default)]
    pub footer_text: Option<String>,
    #[serde(default)]
    pub logo_storage_target: Option<String>,
    #[serde(default)]
    pub logo_storage_key: Option<String>,
    #[serde(default)]
    pub logo_url: Option<String>,
    pub primary_color: String,
    pub accent_color: String,
    pub font_family: String,
    pub columns: Vec<TemplateColumn>,
    pub line_item_layout: Value,
    pub show_tax_breakdown: bool,
    #[serde(default)]
    pub payment_instructions: Option<String>,
    #[serde(default)]
    pub footer_notes: Option<String>,
    pub version: u32,
    #[serde(default)]
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One archived template version: the full JSON snapshot of the
/// template as it was before an edit. Append-only; restoring a
/// version copies it forward as a new version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvoiceTemplateVersion {
    pub id: String,
    pub template_id: String,
    pub version: u32,
    pub snapshot: Value,
    #[serde(default)]
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Input for creating an invoice template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateInvoiceTemplate {
    pub name: String,
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub paper_size_code: Option<String>,
    #[serde(default)]
    pub header_text: Option<String>,
    #[serde(default)]
    pub footer_text: Option<String>,
    #[serde(default)]
    pub logo_storage_target: Option<String>,
    #[serde(default)]
    pub logo_storage_key: Option<String>,
    #[serde(default)]
    pub logo_url: Option<String>,
    #[serde(default)]
    pub primary_color: Option<String>,
    #[serde(default)]
    pub accent_color: Option<String>,
    #[serde(default)]
    pub font_family: Option<String>,
    #[serde(default)]
    pub columns: Option<Vec<TemplateColumnInput>>,
    #[serde(default)]
    pub line_item_layout: Option<Value>,
    #[serde(default)]
    pub show_tax_breakdown: Option<bool>,
    #[serde(default)]
    pub payment_instructions: Option<String>,
    #[serde(default)]
    pub footer_notes: Option<String>,
}

/// Input for updating an invoice template. Every field is optional;
/// only the fields present change. Any successful update archives
/// the previous version and bumps `version` by one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateInvoiceTemplate {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub company_id: Option<Option<String>>,
    #[serde(default)]
    pub paper_size_code: Option<String>,
    #[serde(default)]
    pub header_text: Option<Option<String>>,
    #[serde(default)]
    pub footer_text: Option<Option<String>>,
    #[serde(default)]
    pub logo_storage_target: Option<Option<String>>,
    #[serde(default)]
    pub logo_storage_key: Option<Option<String>>,
    #[serde(default)]
    pub logo_url: Option<Option<String>>,
    #[serde(default)]
    pub primary_color: Option<String>,
    #[serde(default)]
    pub accent_color: Option<String>,
    #[serde(default)]
    pub font_family: Option<String>,
    #[serde(default)]
    pub columns: Option<Vec<TemplateColumnInput>>,
    #[serde(default)]
    pub line_item_layout: Option<Value>,
    #[serde(default)]
    pub show_tax_breakdown: Option<bool>,
    #[serde(default)]
    pub payment_instructions: Option<Option<String>>,
    #[serde(default)]
    pub footer_notes: Option<Option<String>>,
}

/// Input for printing one invoice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrintInvoiceRequest {
    #[serde(default)]
    pub template_id: Option<String>,
    #[serde(default)]
    pub paper_size_code: Option<String>,
    #[serde(default)]
    pub paper_width_mm: Option<f64>,
    #[serde(default)]
    pub paper_height_mm: Option<f64>,
    #[serde(default)]
    pub output_kind: Option<String>,
    #[serde(default)]
    pub printer_profile_id: Option<String>,
}

/// Query for the print-ready document of one invoice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrintReadyQuery {
    #[serde(default)]
    pub template_id: Option<String>,
    #[serde(default)]
    pub paper_size_code: Option<String>,
    #[serde(default)]
    pub paper_width_mm: Option<f64>,
    #[serde(default)]
    pub paper_height_mm: Option<f64>,
    /// Preview only: render this real invoice instead of the
    /// sample invoice.
    #[serde(default)]
    pub invoice_id: Option<String>,
}

/// Input for emailing one invoice as a print-ready document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailInvoiceRequest {
    pub to: String,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub body_text: Option<String>,
    #[serde(default)]
    pub template_id: Option<String>,
    #[serde(default)]
    pub paper_size_code: Option<String>,
}

/// A `#RGB` or `#RRGGBB` colour.
fn valid_hex_color(value: &str) -> bool {
    let hex = value.strip_prefix('#').unwrap_or("");
    (hex.len() == 3 || hex.len() == 6) && hex.chars().all(|c| c.is_ascii_hexdigit())
}

/// A font-family value safe to interpolate into a `<style>` block:
/// letters, digits, spaces, commas, hyphens, and quotes only — no
/// angle brackets, braces, semicolons, slashes, or `url(`.
fn valid_font_family(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric() || c == ' ' || c == ',' || c == '-' || c == '\'' || c == '"'
        })
}

/// A paper-size code is a lowercase slug, matching the printing
/// module's registry codes.
fn valid_paper_size_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// A short free-text template field (header, footer, notes,
/// instructions): at most `max` characters when present.
fn validate_optional_text(
    value: &Option<String>,
    max: usize,
    label: &str,
) -> UnifiedResult<()> {
    if let Some(text) = value {
        if text.len() > max {
            return Err(UnifiedError::bad_request(format!(
                "{label} must be at most {max} characters"
            )));
        }
    }
    Ok(())
}

/// Validate a logo URL: an absolute http(s) URL or a root-relative
/// path, at most 2048 characters.
fn validate_logo_url(value: &Option<String>) -> UnifiedResult<()> {
    if let Some(url) = value {
        if url.len() > 2048
            || !(url.starts_with("https://")
                || url.starts_with("http://")
                || url.starts_with('/'))
        {
            return Err(UnifiedError::bad_request(
                "Logo URL must be an http(s) URL or a root-relative path",
            ));
        }
    }
    Ok(())
}

/// Validate template columns: 1 to 8 columns, keys from the closed
/// vocabulary, no duplicate keys, labels 1 to 60 characters.
fn validate_columns(
    input: &[TemplateColumnInput],
) -> UnifiedResult<Vec<TemplateColumn>> {
    if input.is_empty() || input.len() > 8 {
        return Err(UnifiedError::bad_request(
            "An invoice template needs 1 to 8 columns",
        ));
    }
    let mut seen: Vec<String> = Vec::new();
    let mut columns = Vec::new();
    for column in input {
        if !TEMPLATE_COLUMNS.contains(&column.key.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown invoice column \"{}\"",
                column.key
            )));
        }
        if seen.contains(&column.key) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate invoice column \"{}\"",
                column.key
            )));
        }
        if column.label.trim().is_empty() || column.label.len() > 60 {
            return Err(UnifiedError::bad_request(
                "Column labels must be 1 to 60 characters",
            ));
        }
        seen.push(column.key.clone());
        columns.push(TemplateColumn {
            key: column.key.clone(),
            label: column.label.trim().to_string(),
            visible: column.visible.unwrap_or(true),
        });
    }
    Ok(columns)
}

/// The default columns: item, quantity, unit price, tax, total.
fn default_template_columns() -> Vec<TemplateColumn> {
    vec![
        TemplateColumn { key: "item".to_string(), label: "Item".to_string(), visible: true },
        TemplateColumn { key: "quantity".to_string(), label: "Qty".to_string(), visible: true },
        TemplateColumn { key: "unit_price".to_string(), label: "Unit price".to_string(), visible: true },
        TemplateColumn { key: "tax_amount".to_string(), label: "Tax".to_string(), visible: true },
        TemplateColumn { key: "line_total".to_string(), label: "Total".to_string(), visible: true },
    ]
}

/// The built-in fallback template, used when no stored template
/// applies. It is never stored: it only exists so printing always
/// has a layout.
fn builtin_default_template(company_id: Option<String>) -> InvoiceTemplate {
    let moment = now();
    InvoiceTemplate {
        id: "builtin-default".to_string(),
        company_id,
        name: "Default".to_string(),
        is_default: false,
        paper_size_code: "a4".to_string(),
        header_text: None,
        footer_text: None,
        logo_storage_target: None,
        logo_storage_key: None,
        logo_url: None,
        primary_color: "#111827".to_string(),
        accent_color: "#2563EB".to_string(),
        font_family: "Arial, sans-serif".to_string(),
        columns: default_template_columns(),
        line_item_layout: json!({}),
        show_tax_breakdown: true,
        payment_instructions: None,
        footer_notes: None,
        version: 1,
        created_by: None,
        created_at: moment,
        updated_at: moment,
    }
}

/// Resolve the template that prints an invoice: the company's
/// configured default, then a default template of that company,
/// then the global default, then the built-in fallback.
fn default_template_for(state: &BillingState, company_id: Option<&str>) -> InvoiceTemplate {
    if let Some(company) = company_id {
        if let Some(entry) = state.company_default_templates.get(company) {
            if let Some(template) = state
                .invoice_templates
                .get(entry.value().as_str())
                .map(|entry| entry.value().clone())
            {
                return template;
            }
        }
        let mut company_defaults: Vec<InvoiceTemplate> = state
            .invoice_templates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|template| {
                template.is_default && template.company_id.as_deref() == Some(company)
            })
            .collect();
        company_defaults.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if let Some(template) = company_defaults.into_iter().next() {
            return template;
        }
    }
    let mut global_defaults: Vec<InvoiceTemplate> = state
        .invoice_templates
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|template| template.is_default && template.company_id.is_none())
        .collect();
    global_defaults.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if let Some(template) = global_defaults.into_iter().next() {
        return template;
    }
    builtin_default_template(company_id.map(str::to_string))
}

/// Archive the current shape of a template as a version row.
fn archive_template_version(
    state: &BillingState,
    template: &InvoiceTemplate,
    actor: &str,
) {
    let snapshot = serde_json::to_value(template).unwrap_or(Value::Null);
    let row = InvoiceTemplateVersion {
        id: new_id(),
        template_id: template.id.clone(),
        version: template.version,
        snapshot,
        created_by: Some(actor.to_string()),
        created_at: now(),
    };
    state.template_versions.insert(row.id.clone(), row);
}

/// A deterministic sample invoice, used by the template preview
/// when no real invoice id is given.
fn sample_invoice() -> Invoice {
    let moment = now();
    Invoice {
        id: "sample".to_string(),
        number: "INV-SAMPLE".to_string(),
        reference: reference_id::EXAMPLE_REFERENCE_ID.to_string(),
        order_id: None,
        subscription_id: None,
        user_id: "sample-customer".to_string(),
        company_id: None,
        items: vec![
            InvoiceItem {
                name: "Consulting services".to_string(),
                quantity: 2,
                unit_price: 150.0,
                line_total: 300.0,
                tax_rate: 0.10,
                tax_amount: 30.0,
            },
            InvoiceItem {
                name: "Annual hosting plan".to_string(),
                quantity: 1,
                unit_price: 240.0,
                line_total: 240.0,
                tax_rate: 0.10,
                tax_amount: 24.0,
            },
        ],
        subtotal: 540.0,
        tax_total: 54.0,
        total: 594.0,
        currency: "USD".to_string(),
        status: "issued".to_string(),
        overdue: false,
        tax_inclusive: false,
        amount_paid: 0.0,
        credit_total: 0.0,
        balance: 594.0,
        issue_date: Some(moment),
        due_date: Some(moment + Duration::days(30)),
        payment_terms_days: Some(30),
        paid_at: None,
        notes: Some("Thank you for your business.".to_string()),
        reminders_sent: vec![],
        created_at: moment,
        updated_at: moment,
    }
}

/// One cell of the line-item table, per the template column key.
fn template_cell(item: &InvoiceItem, key: &str) -> String {
    match key {
        "item" => escape_html(&item.name),
        "quantity" => item.quantity.to_string(),
        "unit_price" => format!("{:.2}", item.unit_price),
        "tax_amount" => format!("{:.2}", item.tax_amount),
        "line_total" => format!("{:.2}", item.line_total),
        "tax_rate" => format!("{:.1}%", item.tax_rate * 100.0),
        _ => String::new(),
    }
}

/// Render an invoice with a template as a print-ready HTML
/// document. `@page` carries the chosen paper size so a print or
/// PDF step honours it; every user-controlled string is escaped.
/// The document is HTML, not PDF bytes: binary PDF conversion is
/// a deployment step (headless browser or a configured converter
/// hook).
fn render_print_ready_html(
    invoice: &Invoice,
    template: &InvoiceTemplate,
    width_mm: f64,
    height_mm: f64,
) -> String {
    let mut header_cells = String::new();
    let mut rows = String::new();
    let visible: Vec<&TemplateColumn> = template
        .columns
        .iter()
        .filter(|column| column.visible)
        .collect();
    for column in &visible {
        header_cells.push_str(&format!("<th>{}</th>", escape_html(&column.label)));
    }
    for item in &invoice.items {
        let mut cells = String::new();
        for column in &visible {
            cells.push_str(&format!("<td>{}</td>", template_cell(item, &column.key)));
        }
        rows.push_str(&format!("<tr>{cells}</tr>"));
    }
    let mut tax_rows = String::new();
    if template.show_tax_breakdown {
        let mut by_rate: Vec<(f64, f64)> = Vec::new();
        for item in &invoice.items {
            let rate = item.tax_rate;
            match by_rate.iter_mut().find(|(r, _)| (*r - rate).abs() < f64::EPSILON) {
                Some((_, amount)) => *amount += item.tax_amount,
                None => by_rate.push((rate, item.tax_amount)),
            }
        }
        by_rate.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        for (rate, amount) in by_rate {
            tax_rows.push_str(&format!(
                "<tr><td>Tax {:.1}%</td><td>{:.2}</td></tr>",
                rate * 100.0,
                amount
            ));
        }
    }
    let due = invoice
        .due_date
        .map(|due| due.to_rfc3339())
        .unwrap_or_else(|| "-".to_string());
    let issued = invoice
        .issue_date
        .map(|issued| issued.to_rfc3339())
        .unwrap_or_else(|| "-".to_string());
    let logo = match (&template.logo_url, &template.logo_storage_target, &template.logo_storage_key) {
        (Some(url), _, _) => format!(
            "<img src=\"{}\" alt=\"Logo\" style=\"max-height:64px;max-width:220px;\">",
            escape_html(url)
        ),
        (None, Some(target), Some(key)) => format!(
            "<div class=\"logo-ref\">Logo: {}/{}</div>",
            escape_html(target),
            escape_html(key)
        ),
        _ => String::new(),
    };
    let header = template
        .header_text
        .as_deref()
        .map(|text| format!("<div class=\"header-text\">{}</div>", escape_html(text)))
        .unwrap_or_default();
    let footer = template
        .footer_text
        .as_deref()
        .map(|text| format!("<div class=\"footer-text\">{}</div>", escape_html(text)))
        .unwrap_or_default();
    let payment = template
        .payment_instructions
        .as_deref()
        .map(|text| {
            format!(
                "<section><h2>Payment instructions</h2><p>{}</p></section>",
                escape_html(text)
            )
        })
        .unwrap_or_default();
    let notes = template
        .footer_notes
        .as_deref()
        .map(|text| format!("<p class=\"notes\">{}</p>", escape_html(text)))
        .unwrap_or_default();
    let invoice_notes = invoice
        .notes
        .as_deref()
        .map(|text| format!("<p class=\"notes\">{}</p>", escape_html(text)))
        .unwrap_or_default();
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Invoice {number}</title><style>\
        @page {{ size: {width}mm {height}mm; margin: 10mm; }}\
        body {{ font-family: {font}; color: #111827; }}\
        h1 {{ color: {primary}; }}\
        h2 {{ color: {accent}; }}\
        table {{ width: 100%; border-collapse: collapse; }}\
        th, td {{ border: 1px solid #d1d5db; padding: 6px 8px; text-align: left; }}\
        th {{ background: {primary}; color: #ffffff; }}\
        .totals td {{ border: none; }}\
        .logo-ref {{ color: #6b7280; }}\
        .notes {{ color: #4b5563; }}\
        </style></head><body>{logo}{header}\
        <h1>Invoice {number}</h1>\
        <p>Status: {status}<br>Customer: {user}<br>Currency: {currency}<br>Issued: {issued}<br>Due date: {due}</p>\
        <table><thead><tr>{header_cells}</tr></thead><tbody>{rows}</tbody></table>\
        <table class=\"totals\"><tbody>\
        <tr><td>Subtotal</td><td>{subtotal:.2}</td></tr>{tax_rows}\
        <tr><td>Tax total</td><td>{tax_total:.2}</td></tr>\
        <tr><td><strong>Total</strong></td><td><strong>{total:.2}</strong></td></tr>\
        <tr><td>Paid</td><td>{paid:.2}</td></tr>\
        <tr><td>Balance</td><td>{balance:.2}</td></tr>\
        </tbody></table>{payment}{invoice_notes}{notes}{footer}</body></html>",
        number = escape_html(&invoice.number),
        width = width_mm,
        height = height_mm,
        font = template.font_family,
        primary = template.primary_color,
        accent = template.accent_color,
        logo = logo,
        header = header,
        status = escape_html(&invoice.status),
        user = escape_html(&invoice.user_id),
        currency = escape_html(&invoice.currency),
        issued = issued,
        due = due,
        header_cells = header_cells,
        rows = rows,
        subtotal = invoice.subtotal,
        tax_rows = tax_rows,
        tax_total = invoice.tax_total,
        total = invoice.total,
        paid = invoice.amount_paid,
        balance = invoice.balance,
        payment = payment,
        invoice_notes = invoice_notes,
        notes = notes,
        footer = footer,
    )
}

/// Thermal (receipt) lines for an invoice, as JSON values shaped
/// like the printing module's `ThermalLine` records. Used when an
/// invoice is printed to a thermal printer.
fn invoice_thermal_lines(invoice: &Invoice) -> Value {
    let mut lines: Vec<Value> = vec![
        json!({ "text": format!("INVOICE {}", invoice.number), "align": "center", "bold": true, "double_size": true }),
        json!({ "text": format!("Status: {}", invoice.status), "align": "left", "bold": false, "double_size": false }),
        json!({ "text": format!("Customer: {}", invoice.user_id), "align": "left", "bold": false, "double_size": false }),
        json!({ "text": "----------------------------------------", "align": "left", "bold": false, "double_size": false }),
    ];
    for item in &invoice.items {
        lines.push(json!({
            "text": format!("{} x{}  {:.2}", item.name, item.quantity, item.line_total),
            "align": "left", "bold": false, "double_size": false,
        }));
    }
    lines.push(json!({ "text": "----------------------------------------", "align": "left", "bold": false, "double_size": false }));
    lines.push(json!({ "text": format!("Subtotal: {:.2} {}", invoice.subtotal, invoice.currency), "align": "right", "bold": false, "double_size": false }));
    lines.push(json!({ "text": format!("Tax: {:.2} {}", invoice.tax_total, invoice.currency), "align": "right", "bold": false, "double_size": false }));
    lines.push(json!({ "text": format!("TOTAL: {:.2} {}", invoice.total, invoice.currency), "align": "right", "bold": true, "double_size": false }));
    if let Some(due) = invoice.due_date {
        lines.push(json!({ "text": format!("Due: {}", due.to_rfc3339()), "align": "left", "bold": false, "double_size": false }));
    }
    Value::Array(lines)
}

/// Validate an explicit paper size in millimetres: 40mm to
/// 1200mm per side.
fn validate_paper_dimensions(width_mm: f64, height_mm: f64) -> UnifiedResult<()> {
    if !width_mm.is_finite() || !(40.0..=1200.0).contains(&width_mm) {
        return Err(UnifiedError::bad_request(
            "Paper width must be between 40mm and 1200mm",
        ));
    }
    if !height_mm.is_finite() || !(40.0..=1200.0).contains(&height_mm) {
        return Err(UnifiedError::bad_request(
            "Paper height must be between 40mm and 1200mm",
        ));
    }
    Ok(())
}

/// A plain email address check: one `@`, non-empty local and
/// domain parts, a dot in the domain, no whitespace.
fn valid_email(value: &str) -> bool {
    if value.len() < 3 || value.len() > 254 || value.chars().any(char::is_whitespace) {
        return false;
    }
    let mut parts = value.split('@');
    let local = parts.next().unwrap_or("");
    let domain = parts.next().unwrap_or("");
    parts.next().is_none() && !local.is_empty() && domain.contains('.') && !domain.is_empty()
}

// ---------------------------------------------------------------------------
// Invoice-template handlers
// ---------------------------------------------------------------------------

async fn list_invoice_templates(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<InvoiceTemplate>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_TEMPLATE_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<InvoiceTemplate> = state
            .invoice_templates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|template| {
                matches_search(&format!("{} {}", template.name, template.id), &params)
                    && params.filters.get("company_id").is_none_or(|company_id| {
                        template.company_id.as_deref() == Some(company_id.as_str())
                            || template.company_id.is_none()
                    })
                    && params
                        .filters
                        .get("is_default")
                        .is_none_or(|flag| template.is_default.to_string() == *flag)
                    && params
                        .filters
                        .get("paper_size_code")
                        .is_none_or(|code| template.paper_size_code == *code)
            })
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn create_invoice_template(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateInvoiceTemplate>,
) -> HttpResponse {
    let result: UnifiedResult<InvoiceTemplate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_TEMPLATE_MANAGE)?;
        let input = body.into_inner();
        if input.name.trim().is_empty() || input.name.len() > 120 {
            return Err(UnifiedError::bad_request(
                "Template name must be 1 to 120 characters",
            ));
        }
        let paper_size_code = input
            .paper_size_code
            .clone()
            .unwrap_or_else(|| "a4".to_string());
        if !valid_paper_size_code(&paper_size_code) {
            return Err(UnifiedError::bad_request(
                "Paper size code must be a lowercase slug",
            ));
        }
        if let Some(color) = &input.primary_color {
            if !valid_hex_color(color) {
                return Err(UnifiedError::bad_request(
                    "Primary colour must be a #RGB or #RRGGBB hex colour",
                ));
            }
        }
        if let Some(color) = &input.accent_color {
            if !valid_hex_color(color) {
                return Err(UnifiedError::bad_request(
                    "Accent colour must be a #RGB or #RRGGBB hex colour",
                ));
            }
        }
        if let Some(font) = &input.font_family {
            if !valid_font_family(font) {
                return Err(UnifiedError::bad_request(
                    "Font family contains characters that are not allowed in a style block",
                ));
            }
        }
        validate_optional_text(&input.header_text, 5000, "Header text")?;
        validate_optional_text(&input.footer_text, 5000, "Footer text")?;
        validate_optional_text(
            &input.payment_instructions,
            5000,
            "Payment instructions",
        )?;
        validate_optional_text(&input.footer_notes, 5000, "Footer notes")?;
        validate_logo_url(&input.logo_url)?;
        for (label, value) in [
            ("Logo storage target", &input.logo_storage_target),
            ("Logo storage key", &input.logo_storage_key),
        ] {
            if let Some(text) = value {
                if text.trim().is_empty() || text.len() > 255 {
                    return Err(UnifiedError::bad_request(format!(
                        "{label} must be 1 to 255 characters"
                    )));
                }
            }
        }
        let columns = match input.columns {
            Some(columns) => validate_columns(&columns)?,
            None => default_template_columns(),
        };
        if let Some(layout) = &input.line_item_layout {
            if !layout.is_object() || layout.to_string().len() > 8000 {
                return Err(UnifiedError::bad_request(
                    "Line-item layout must be a JSON object of at most 8000 characters",
                ));
            }
        }
        let moment = now();
        let template = InvoiceTemplate {
            id: new_id(),
            company_id: input.company_id.clone(),
            name: input.name.trim().to_string(),
            is_default: false,
            paper_size_code,
            header_text: input.header_text,
            footer_text: input.footer_text,
            logo_storage_target: input.logo_storage_target,
            logo_storage_key: input.logo_storage_key,
            logo_url: input.logo_url,
            primary_color: input
                .primary_color
                .unwrap_or_else(|| "#111827".to_string()),
            accent_color: input
                .accent_color
                .unwrap_or_else(|| "#2563EB".to_string()),
            font_family: input
                .font_family
                .unwrap_or_else(|| "Arial, sans-serif".to_string()),
            columns,
            line_item_layout: input.line_item_layout.unwrap_or_else(|| json!({})),
            show_tax_breakdown: input.show_tax_breakdown.unwrap_or(true),
            payment_instructions: input.payment_instructions,
            footer_notes: input.footer_notes,
            version: 1,
            created_by: Some(caller.actor.clone()),
            created_at: moment,
            updated_at: moment,
        };
        state
            .invoice_templates
            .insert(template.id.clone(), template.clone());
        archive_template_version(&state, &template, &caller.actor);
        state.emit(
            EVENT_INVOICE_TEMPLATE_CHANGED,
            json!({
                "template_id": template.id.clone(),
                "company_id": template.company_id.clone(),
                "version": template.version,
                "action": "created",
                "updated_by": caller.actor,
            }),
        );
        Ok(template)
    })();
    match result {
        Ok(template) => json::success_response(template, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn get_invoice_template(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<InvoiceTemplate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_TEMPLATE_VIEW)?;
        state
            .invoice_templates
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice template not found"))
    })();
    json::respond(result, false)
}

async fn update_invoice_template(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateInvoiceTemplate>,
) -> HttpResponse {
    let result: UnifiedResult<InvoiceTemplate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_TEMPLATE_MANAGE)?;
        let id = path.into_inner();
        let mut template = state
            .invoice_templates
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice template not found"))?;
        // Archive the pre-edit shape first: the version row must
        // hold the template as it was before this update.
        archive_template_version(&state, &template, &caller.actor);
        let input = body.into_inner();
        if let Some(name) = input.name {
            if name.trim().is_empty() || name.len() > 120 {
                return Err(UnifiedError::bad_request(
                    "Template name must be 1 to 120 characters",
                ));
            }
            template.name = name.trim().to_string();
        }
        if let Some(company_id) = input.company_id {
            template.company_id = company_id;
        }
        if let Some(code) = input.paper_size_code {
            if !valid_paper_size_code(&code) {
                return Err(UnifiedError::bad_request(
                    "Paper size code must be a lowercase slug",
                ));
            }
            template.paper_size_code = code;
        }
        if let Some(header_text) = input.header_text {
            validate_optional_text(&header_text, 5000, "Header text")?;
            template.header_text = header_text;
        }
        if let Some(footer_text) = input.footer_text {
            validate_optional_text(&footer_text, 5000, "Footer text")?;
            template.footer_text = footer_text;
        }
        if let Some(target) = input.logo_storage_target {
            if let Some(text) = &target {
                if text.trim().is_empty() || text.len() > 255 {
                    return Err(UnifiedError::bad_request(
                        "Logo storage target must be 1 to 255 characters",
                    ));
                }
            }
            template.logo_storage_target = target;
        }
        if let Some(key) = input.logo_storage_key {
            if let Some(text) = &key {
                if text.trim().is_empty() || text.len() > 255 {
                    return Err(UnifiedError::bad_request(
                        "Logo storage key must be 1 to 255 characters",
                    ));
                }
            }
            template.logo_storage_key = key;
        }
        if let Some(url) = input.logo_url {
            validate_logo_url(&url)?;
            template.logo_url = url;
        }
        if let Some(color) = input.primary_color {
            if !valid_hex_color(&color) {
                return Err(UnifiedError::bad_request(
                    "Primary colour must be a #RGB or #RRGGBB hex colour",
                ));
            }
            template.primary_color = color;
        }
        if let Some(color) = input.accent_color {
            if !valid_hex_color(&color) {
                return Err(UnifiedError::bad_request(
                    "Accent colour must be a #RGB or #RRGGBB hex colour",
                ));
            }
            template.accent_color = color;
        }
        if let Some(font) = input.font_family {
            if !valid_font_family(&font) {
                return Err(UnifiedError::bad_request(
                    "Font family contains characters that are not allowed in a style block",
                ));
            }
            template.font_family = font;
        }
        if let Some(columns) = input.columns {
            template.columns = validate_columns(&columns)?;
        }
        if let Some(layout) = input.line_item_layout {
            if !layout.is_object() || layout.to_string().len() > 8000 {
                return Err(UnifiedError::bad_request(
                    "Line-item layout must be a JSON object of at most 8000 characters",
                ));
            }
            template.line_item_layout = layout;
        }
        if let Some(show) = input.show_tax_breakdown {
            template.show_tax_breakdown = show;
        }
        if let Some(instructions) = input.payment_instructions {
            validate_optional_text(&instructions, 5000, "Payment instructions")?;
            template.payment_instructions = instructions;
        }
        if let Some(notes) = input.footer_notes {
            validate_optional_text(&notes, 5000, "Footer notes")?;
            template.footer_notes = notes;
        }
        // The pre-edit shape was archived above; bump the version.
        template.version += 1;
        template.updated_at = now();
        state
            .invoice_templates
            .insert(id.clone(), template.clone());
        state.emit(
            EVENT_INVOICE_TEMPLATE_CHANGED,
            json!({
                "template_id": template.id.clone(),
                "company_id": template.company_id.clone(),
                "version": template.version,
                "action": "updated",
                "updated_by": caller.actor,
            }),
        );
        Ok(template)
    })();
    json::respond(result, false)
}

async fn delete_invoice_template(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_TEMPLATE_MANAGE)?;
        let id = path.into_inner();
        let template = state
            .invoice_templates
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice template not found"))?;
        if let Some(company) = &template.company_id {
            if let Some(default_id) = state.company_default_templates.get(company) {
                if default_id.value().as_str() == id {
                    state.company_default_templates.remove(company);
                }
            }
        }
        state.invoice_templates.remove(&id);
        state
            .template_versions
            .retain(|_, row| row.template_id != id);
        state.emit(
            EVENT_INVOICE_TEMPLATE_CHANGED,
            json!({
                "template_id": id.clone(),
                "company_id": template.company_id,
                "version": template.version,
                "action": "deleted",
                "updated_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, false)
}

async fn list_invoice_template_versions(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<InvoiceTemplateVersion>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_TEMPLATE_VIEW)?;
        let id = path.into_inner();
        if !state.invoice_templates.contains_key(&id) {
            return Err(UnifiedError::not_found("Invoice template not found"));
        }
        let params = query.into_inner();
        let mut items: Vec<InvoiceTemplateVersion> = state
            .template_versions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.template_id == id)
            .collect();
        items.sort_by(|a, b| a.version.cmp(&b.version));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn restore_invoice_template_version(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, u32)>,
) -> HttpResponse {
    let result: UnifiedResult<InvoiceTemplate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_TEMPLATE_MANAGE)?;
        let (id, version) = path.into_inner();
        let mut current = state
            .invoice_templates
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice template not found"))?;
        let row = state
            .template_versions
            .iter()
            .map(|entry| entry.value().clone())
            .find(|row| row.template_id == id && row.version == version)
            .ok_or_else(|| {
                UnifiedError::not_found("Template version not found")
            })?;
        let mut restored: InvoiceTemplate =
            serde_json::from_value(row.snapshot.clone()).map_err(|_| {
                UnifiedError::internal("Stored template version cannot be read")
            })?;
        // Restoring copies the old shape forward as a new version:
        // history is never rewritten, and the default flag of the
        // live template is preserved.
        archive_template_version(&state, &current, &caller.actor);
        restored.id = current.id.clone();
        restored.is_default = current.is_default;
        restored.version = current.version + 1;
        restored.updated_at = now();
        current = restored;
        state
            .invoice_templates
            .insert(id.clone(), current.clone());
        state.emit(
            EVENT_INVOICE_TEMPLATE_CHANGED,
            json!({
                "template_id": current.id.clone(),
                "company_id": current.company_id.clone(),
                "version": current.version,
                "action": "restored",
                "restored_from_version": version,
                "updated_by": caller.actor,
            }),
        );
        Ok(current)
    })();
    json::respond(result, false)
}

async fn set_default_invoice_template(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<Value>,
) -> HttpResponse {
    let result: UnifiedResult<InvoiceTemplate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_TEMPLATE_MANAGE)?;
        let id = path.into_inner();
        let mut template = state
            .invoice_templates
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice template not found"))?;
        let company_id = body
            .get("company_id")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if company_id.is_empty() {
            return Err(UnifiedError::bad_request("company_id must not be empty"));
        }
        // Clear the default flag on the company's other templates.
        // A global template keeps its own flag untouched: the
        // per-company mapping below is the source of truth for it.
        for mut entry in state.invoice_templates.iter_mut() {
            if entry.company_id.as_deref() == Some(company_id.as_str()) && entry.id != id {
                entry.is_default = false;
                entry.updated_at = now();
            }
        }
        if template.company_id.as_deref() == Some(company_id.as_str()) {
            template.is_default = true;
        }
        template.updated_at = now();
        state
            .company_default_templates
            .insert(company_id.clone(), id.clone());
        state
            .invoice_templates
            .insert(id.clone(), template.clone());
        state.emit(
            EVENT_INVOICE_TEMPLATE_CHANGED,
            json!({
                "template_id": template.id.clone(),
                "company_id": company_id.clone(),
                "version": template.version,
                "action": "set_default",
                "updated_by": caller.actor,
            }),
        );
        Ok(template)
    })();
    json::respond(result, false)
}

async fn preview_invoice_template(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PrintReadyQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_TEMPLATE_VIEW)?;
        let id = path.into_inner();
        let template = state
            .invoice_templates
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice template not found"))?;
        let query = query.into_inner();
        let width_mm = query.paper_width_mm.unwrap_or(210.0);
        let height_mm = query.paper_height_mm.unwrap_or(297.0);
        validate_paper_dimensions(width_mm, height_mm)?;
        // The preview renders a real invoice when `invoice_id` is
        // given, otherwise sample data.
        let (invoice, sample) = match query.invoice_id {
            Some(invoice_id) => {
                let found = state
                    .invoices
                    .get(&invoice_id)
                    .map(|entry| view_invoice(entry.value()))
                    .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
                (found, false)
            }
            None => (sample_invoice(), true),
        };
        let html = render_print_ready_html(&invoice, &template, width_mm, height_mm);
        Ok(json!({
            "template_id": template.id,
            "template_version": template.version,
            "invoice_id": invoice.id,
            "sample": sample,
            "paper_size_code": template.paper_size_code,
            "paper_width_mm": width_mm,
            "paper_height_mm": height_mm,
            "html": html,
        }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Invoice printing handlers
// ---------------------------------------------------------------------------

/// Resolve the template for a print: an explicit template id, or
/// the resolved default for the invoice's company.
fn resolve_print_template(
    state: &BillingState,
    template_id: Option<String>,
    company_id: Option<&str>,
) -> UnifiedResult<InvoiceTemplate> {
    match template_id {
        Some(id) => state
            .invoice_templates
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice template not found")),
        None => Ok(default_template_for(state, company_id)),
    }
}

/// Resolve the paper size and dimensions for a print: an explicit
/// size code (or the template's), and explicit millimetre
/// dimensions (or A4).
fn resolve_print_paper(
    request_code: Option<String>,
    template: &InvoiceTemplate,
    width_mm: Option<f64>,
    height_mm: Option<f64>,
) -> UnifiedResult<(String, f64, f64)> {
    let code = request_code.unwrap_or_else(|| template.paper_size_code.clone());
    if !valid_paper_size_code(&code) {
        return Err(UnifiedError::bad_request(
            "Paper size code must be a lowercase slug",
        ));
    }
    let width = width_mm.unwrap_or(210.0);
    let height = height_mm.unwrap_or(297.0);
    validate_paper_dimensions(width, height)?;
    Ok((code, width, height))
}

/// Queue a print job through the printing module and return the
/// `(job_id, status)` it answers with. Fails when the printing
/// module has no `printing.job.requested` handler (it is not
/// registered) or when it reports an error.
fn request_print_job(
    state: &BillingState,
    payload: Value,
) -> UnifiedResult<(String, String)> {
    if !state.bus().has("printing.job.requested") {
        return Err(UnifiedError::internal(
            "Printing module is not registered: printing.job.requested has no handler",
        ));
    }
    let context = state.bus().emit("printing.job.requested", payload);
    match context.get("job_id").and_then(|value| value.as_str()) {
        Some(job_id) => {
            let status = context
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("queued")
                .to_string();
            Ok((job_id.to_string(), status))
        }
        None => {
            let detail = context
                .get("error")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown error")
                .to_string();
            Err(UnifiedError::internal(format!(
                "Printing module did not accept the print job: {detail}"
            )))
        }
    }
}

async fn print_invoice(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PrintInvoiceRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_PRINT)?;
        let id = path.into_inner();
        let invoice = state
            .invoices
            .get(&id)
            .map(|entry| view_invoice(entry.value()))
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        let input = body.into_inner();
        let template = resolve_print_template(
            &state,
            input.template_id.clone(),
            invoice.company_id.as_deref(),
        )?;
        let (paper_size_code, width_mm, height_mm) = resolve_print_paper(
            input.paper_size_code.clone(),
            &template,
            input.paper_width_mm,
            input.paper_height_mm,
        )?;
        let output_kind = input
            .output_kind
            .clone()
            .unwrap_or_else(|| "paper".to_string());
        if !["paper", "thermal", "pdf", "email"].contains(&output_kind.as_str()) {
            return Err(UnifiedError::bad_request(
                "Print output kind must be paper, thermal, pdf, or email",
            ));
        }
        let mut payload = json!({
            "invoice_id": invoice.id.clone(),
            "company_id": invoice.company_id.clone(),
            "template_id": template.id.clone(),
            "template_version": template.version,
            "paper_size_code": paper_size_code.clone(),
            "output_kind": output_kind.clone(),
            "printer_profile_id": input.printer_profile_id.clone(),
            "requested_by": caller.actor.clone(),
        });
        if output_kind == "thermal" {
            payload["thermal_lines"] = invoice_thermal_lines(&invoice);
        } else {
            payload["document_html"] =
                json!(render_print_ready_html(&invoice, &template, width_mm, height_mm));
        }
        let (job_id, status) = request_print_job(&state, payload)?;
        state.emit(
            EVENT_INVOICE_PRINTED,
            json!({
                "invoice_id": invoice.id.clone(),
                "invoice_number": invoice.number.clone(),
                "job_id": job_id,
                "template_id": template.id.clone(),
                "template_version": template.version,
                "output_kind": output_kind.clone(),
                "requested_by": caller.actor,
            }),
        );
        Ok(json!({
            "invoice_id": invoice.id.clone(),
            "invoice_number": invoice.number,
            "job_id": job_id,
            "status": status,
            "output_kind": output_kind,
            "template_id": template.id,
            "template_version": template.version,
            "paper_size_code": paper_size_code,
            "print_ready_url": format!("/api/v1/billing/invoices/{}/print-ready", invoice.id),
        }))
    })();
    json::respond(result, false)
}

async fn print_ready_invoice(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PrintReadyQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_PRINT)?;
        let id = path.into_inner();
        let invoice = state
            .invoices
            .get(&id)
            .map(|entry| view_invoice(entry.value()))
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        let query = query.into_inner();
        let template = resolve_print_template(
            &state,
            query.template_id.clone(),
            invoice.company_id.as_deref(),
        )?;
        let (paper_size_code, width_mm, height_mm) = resolve_print_paper(
            query.paper_size_code.clone(),
            &template,
            query.paper_width_mm,
            query.paper_height_mm,
        )?;
        let html = render_print_ready_html(&invoice, &template, width_mm, height_mm);
        Ok(json!({
            "invoice_id": invoice.id,
            "invoice_number": invoice.number,
            "template_id": template.id,
            "template_version": template.version,
            "paper_size_code": paper_size_code,
            "paper_width_mm": width_mm,
            "paper_height_mm": height_mm,
            "html": html,
            "note": "Print-ready HTML document with @page rules for the chosen paper size. Binary PDF conversion is a deployment step (headless browser or a configured converter hook), not a PDF binary.",
        }))
    })();
    json::respond(result, false)
}

async fn list_invoice_print_jobs(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_PRINT)?;
        let id = path.into_inner();
        if !state.invoices.contains_key(&id) {
            return Err(UnifiedError::not_found("Invoice not found"));
        }
        if !state.bus().has("printing.jobs.list_requested") {
            return Ok(json!({
                "invoice_id": id,
                "jobs": [],
                "total": 0,
                "printing_available": false,
            }));
        }
        let status_filter = query
            .into_inner()
            .filters
            .get("status")
            .cloned();
        let mut payload = json!({ "invoice_id": id.clone() });
        if let Some(status) = status_filter {
            payload["status"] = json!(status);
        }
        let context = state.bus().emit("printing.jobs.list_requested", payload);
        let jobs = context.get("jobs").cloned().unwrap_or_else(|| json!([]));
        let total = context
            .get("total")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        Ok(json!({
            "invoice_id": id,
            "jobs": jobs,
            "total": total,
            "printing_available": true,
        }))
    })();
    json::respond(result, false)
}

async fn reprint_invoice_print_job(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_PRINT)?;
        let job_id = path.into_inner();
        if !state.bus().has("printing.job.reprint_requested") {
            return Err(UnifiedError::internal(
                "Printing module is not registered: printing.job.reprint_requested has no handler",
            ));
        }
        let context = state.bus().emit(
            "printing.job.reprint_requested",
            json!({ "job_id": job_id.clone(), "requested_by": caller.actor }),
        );
        match context.get("job_id").and_then(|value| value.as_str()) {
            Some(new_job_id) => {
                let status = context
                    .get("status")
                    .and_then(|value| value.as_str())
                    .unwrap_or("queued")
                    .to_string();
                Ok(json!({
                    "job_id": new_job_id,
                    "previous_job_id": job_id,
                    "status": status,
                }))
            }
            None => {
                let detail = context
                    .get("error")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown error")
                    .to_string();
                Err(UnifiedError::internal(format!(
                    "Printing module did not accept the reprint: {detail}"
                )))
            }
        }
    })();
    json::respond(result, false)
}

async fn email_invoice_pdf(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<EmailInvoiceRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_PRINT)?;
        let id = path.into_inner();
        let invoice = state
            .invoices
            .get(&id)
            .map(|entry| view_invoice(entry.value()))
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        let input = body.into_inner();
        if !valid_email(&input.to) {
            return Err(UnifiedError::bad_request(
                "Recipient email address is not valid",
            ));
        }
        let template = resolve_print_template(
            &state,
            input.template_id.clone(),
            invoice.company_id.as_deref(),
        )?;
        let (paper_size_code, width_mm, height_mm) = resolve_print_paper(
            input.paper_size_code.clone(),
            &template,
            None,
            None,
        )?;
        let html = render_print_ready_html(&invoice, &template, width_mm, height_mm);
        let subject = input
            .subject
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| format!("Invoice {}", invoice.number));
        if subject.len() > 200 {
            return Err(UnifiedError::bad_request(
                "Email subject must be at most 200 characters",
            ));
        }
        let body_text = input.body_text.filter(|text| !text.trim().is_empty()).unwrap_or_else(|| {
            format!(
                "Invoice {} for {:.2} {} is attached as a print-ready document. Balance due: {:.2} {}.",
                invoice.number, invoice.total, invoice.currency, invoice.balance, invoice.currency
            )
        });
        // The mail module has no attachment field: the print-ready
        // document travels as the HTML body of the message.
        let mail_context = state.bus().emit(
            "mail.send_requested",
            json!({
                "to": input.to.clone(),
                "subject": subject,
                "body_text": body_text,
                "body_html": html,
                "variables": {
                    "invoice_id": invoice.id.clone(),
                    "invoice_number": invoice.number.clone(),
                },
            }),
        );
        let mail_job_id = mail_context
            .results
            .iter()
            .find_map(|result| {
                result
                    .get("job_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            });
        state.emit(
            EVENT_INVOICE_EMAILED,
            json!({
                "invoice_id": invoice.id.clone(),
                "invoice_number": invoice.number.clone(),
                "to": input.to.clone(),
                "mail_job_id": mail_job_id,
                "paper_size_code": paper_size_code,
                "requested_by": caller.actor,
            }),
        );
        Ok(json!({
            "invoice_id": invoice.id,
            "invoice_number": invoice.number,
            "to": input.to,
            "mailed": true,
            "mail_job_id": mail_job_id,
            "template_id": template.id,
            "template_version": template.version,
            "note": "The mail module has no attachment field, so the print-ready document was sent as the HTML body of the message.",
        }))
    })();
    json::respond(result, false)
}
