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

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
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
    pub order_id: Option<String>,
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
            invoice_seq: AtomicU64::new(1),
            credit_seq: AtomicU64::new(1),
            processed_payment_events: DashMap::new(),
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
            order_id: Some(order_id),
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
        let invoice = Invoice {
            id: new_id(),
            number: state.next_invoice_number(),
            order_id: input.order_id,
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
        Ok(state
            .invoices
            .get(path.as_str())
            .map(|entry| view_invoice(entry.value()))
            .expect("invoice still present"))
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
    let order_id = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .or_else(|| {
            ctx.get("order")
                .and_then(|order| order.get("id"))
                .and_then(|id| id.as_str())
        })?;
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
        "Billing and invoicing: invoices from orders, numbered invoices with taxes and due dates, payments, credit notes, overdue reminders and PDF-ready data."
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
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/overdue", web::get().to(list_overdue)),
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
            RouteInfo { method: "GET", path: "/api/v1/billing/settings", owner: "billing", description: "Read billing settings (numbering, taxes, terms, reminders)." },
            RouteInfo { method: "PUT", path: "/api/v1/billing/settings", owner: "billing", description: "Update billing settings." },
            RouteInfo { method: "GET", path: "/api/v1/billing/overdue", owner: "billing", description: "List overdue invoices." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(BillingModule::new()));
}
