//! billing - types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Capability checked for `GET /api/v1/billing/settings`.
pub(crate) const PERM_BILLING_SETTINGS_VIEW: &str = "billing.settings.view";
/// Capability checked for `PUT /api/v1/billing/settings`.
pub(crate) const PERM_BILLING_SETTINGS_MANAGE: &str = "billing.settings.manage";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvoiceItem {
    pub name: String,
    pub quantity: u32,
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub unit_price: i64,
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub line_total: i64,
    /// Tax rate applied to this line, as a fraction (0.10 means 10%).
    #[serde(default)]
    pub tax_rate: f64,
    /// Tax amount for this line. `line_total` is the net amount when
    /// tax is exclusive and the gross amount when tax is inclusive;
    /// either way `subtotal + tax_total` equals the invoice total.
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(default, with = "unified_core::money::serde_major")]
    pub tax_amount: i64,
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
    /// see `unified_core::reference_id`). An invoice built from an
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
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(default, with = "unified_core::money::serde_major")]
    pub subtotal: i64,
    /// Total tax across all lines.
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(default, with = "unified_core::money::serde_major")]
    pub tax_total: i64,
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub total: i64,
    pub currency: String,
    /// Base status: `draft`, `issued`, `paid`, `partially_paid`,
    /// `partially_refunded`, `unpaid`, `void`, `credited`. The serialized status is the
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
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(default, with = "unified_core::money::serde_major")]
    pub amount_paid: i64,
    /// Amount credited so far across all credit notes.
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(default, with = "unified_core::money::serde_major")]
    pub credit_total: i64,
    /// Outstanding amount: `total - amount_paid - credit_total`.
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(default, with = "unified_core::money::serde_major")]
    pub balance: i64,
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
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub amount: i64,
    pub method: String,
    #[serde(default)]
    pub reference: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

/// One expense-reimbursement disbursement recorded by billing.
/// Billing holds no gateway payout integration, so the disbursement
/// is recorded here in billing's own ledger as the payment of
/// record -- the same manual-disbursement model as `pay_invoice` --
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
/// the step that flips the store-side record). Amounts are integer
/// minor units, matching the store's `DigitalPayoutRequest`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingDigitalPayout {
    /// The store payout request id.
    pub id: String,
    #[serde(default)]
    pub payee: Option<String>,
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub amount: i64,
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
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub amount: i64,
    pub reason: String,
    pub created_at: DateTime<Utc>,
}

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
/// status change on its own -- decisions go out as
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
    /// `trialing`, `active`, `past_due`, `cancelled`, or `expired`
    /// (set by the store expiry sweep once the access window closes).
    pub status: String,
    pub billing_period: String,
    /// Price charged per period, in integer minor units.
    /// Serialized as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub price: i64,
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
    /// Integer minor units when present; accepted as major units in JSON.
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub total: Option<i64>,
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
    /// Integer minor units; accepted as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub unit_price: i64,
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
    /// Integer minor units; accepted as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub amount: i64,
    pub method: Option<String>,
    #[serde(default)]
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateCreditNoteRequest {
    /// Integer minor units; accepted as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub amount: i64,
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
