//! billing - state.

use super::helpers::*;
use super::reference::*;
use super::school_fees_types::*;
use super::templates::*;
use super::types::*;
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use serde_json::{Value, json};
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicU64, Ordering},
};
use unified_core::db::{
    DbPool,
    models::{
        CreditNote as DbCreditNote, Invoice as DbInvoice, InvoiceItem as DbInvoiceItem,
        InvoicePayment as DbInvoicePayment, InvoiceSequence as DbInvoiceSequence,
    },
    money::pg_numeric_to_minor,
};
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::reference_id::ReferenceIdSettings;
use uuid::Uuid;

pub struct BillingState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) invoices: DashMap<String, Invoice>,
    /// Orders seen through the `order.created` hook, keyed by order id,
    /// so `from-order` can build an invoice from real order data.
    pub(crate) seen_orders: DashMap<String, Value>,
    pub(crate) payments: DashMap<String, InvoicePayment>,
    pub(crate) credit_notes: DashMap<String, CreditNote>,
    pub(crate) settings: RwLock<BillingSettings>,
    /// Admin-editable payment reference settings (default prefix and
    /// per-product prefix map; see `unified_core::reference_id`).
    /// Fresh invoice references are issued from these settings;
    /// order-derived invoices reuse the order's reference instead.
    /// Changed through `GET/PUT /api/v1/billing/reference-settings`.
    pub(crate) reference_settings: RwLock<ReferenceIdSettings>,
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
    pub(crate) processed_payment_events: DashMap<String, DateTime<Utc>>,
    /// Subscription lifecycle mirrors, keyed by subscription id.
    /// Synced from the store's `subscription.*` events.
    pub(crate) subscription_lifecycles: DashMap<String, SubscriptionLifecycle>,
    /// Recorded failed renewal payments, keyed by attempt id.
    pub(crate) dunning_attempts: DashMap<String, DunningAttempt>,
    /// Expense-reimbursement disbursements recorded by billing,
    /// keyed by reimbursement id. See `ReimbursementPayout`.
    pub(crate) reimbursement_payouts: DashMap<String, ReimbursementPayout>,
    /// Digital-product royalty payouts recorded by billing, keyed
    /// by the store payout request id. See `BillingDigitalPayout`.
    pub(crate) digital_payouts: DashMap<String, BillingDigitalPayout>,
    /// Invoice templates, keyed by template id. Templates are
    /// data: every edit archives the previous version first.
    pub(crate) invoice_templates: DashMap<String, InvoiceTemplate>,
    /// Archived template versions, keyed by version-row id.
    pub(crate) template_versions: DashMap<String, InvoiceTemplateVersion>,
    /// Default invoice template per company, keyed by company id.
    pub(crate) company_default_templates: DashMap<String, String>,
    // --- School fees (see the school-fees section at the end of
    // this file). Money is kept in integer minor units here, never
    // floats; the linked billing invoice is the only place a fee
    // amount is also carried as a major-unit float, converted at
    // the boundary by `minor_to_major` / `major_to_minor`.
    /// Admin-editable fee type definitions, keyed by fee type id.
    pub(crate) fee_types: DashMap<String, SchoolFeeType>,
    /// Per-student fee schedules, keyed by schedule id.
    pub(crate) fee_schedules: DashMap<String, StudentFeeSchedule>,
    /// Generated fee charges, keyed by charge id.
    pub(crate) fee_charges: DashMap<String, FeeCharge>,
    /// Per-student discounts and scholarships, keyed by discount id.
    pub(crate) fee_discounts: DashMap<String, FeeDiscount>,
    /// Issued fee receipts, keyed by receipt id.
    pub(crate) fee_receipts: DashMap<String, FeeReceipt>,
    /// Guardian-to-student links, keyed by link id.
    pub(crate) fee_guardians: DashMap<String, FeeGuardianLink>,
    /// Fee reminders already sent, keyed by
    /// `{charge_id}:{offset_days}`.
    pub(crate) fee_reminders: DashMap<String, FeeReminder>,
    /// School fee settings (configurable defaults).
    pub(crate) fee_settings: RwLock<SchoolFeeSettings>,
    /// Monotonic fee receipt number sequence.
    pub(crate) fee_receipt_seq: AtomicU64,
}

impl BillingState {
    pub(crate) fn new() -> Self {
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

    pub(crate) fn settings(&self) -> BillingSettings {
        self.settings
            .read()
            .expect("billing settings lock poisoned")
            .clone()
    }

    /// Next invoice number: configured prefix plus a zero-padded
    /// sequence that is never reused.
    pub(crate) fn next_invoice_number(&self) -> String {
        let prefix = self.settings().invoice_prefix;
        let seq = self.invoice_seq.fetch_add(1, Ordering::SeqCst);
        format!("{prefix}-{seq:06}")
    }

    pub(crate) fn next_credit_note_number(&self) -> String {
        let seq = self.credit_seq.fetch_add(1, Ordering::SeqCst);
        format!("CN-{seq:06}")
    }

    /// Preview of the next invoice number without consuming it.
    pub(crate) fn next_invoice_number_preview(&self) -> String {
        let prefix = self.settings().invoice_prefix;
        let seq = self.invoice_seq.load(Ordering::SeqCst);
        format!("{prefix}-{seq:06}")
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Hydrate the in-memory billing maps from the database (C11,
    /// task 271): invoices (with their items), invoice payments,
    /// and credit notes are loaded from the `invoices`,
    /// `invoice_items`, `invoice_payments`, and `credit_notes`
    /// tables and replace the map contents, so a restart restores
    /// the money state instead of losing it. The invoice and
    /// credit-note number sequences are reseeded from the
    /// `invoice_sequences` table when its rows exist (falling
    /// back to the largest loaded number), so numbers are never
    /// reused after a restart. Rows that cannot be converted —
    /// an amount that does not fit in minor units, a payment or
    /// credit note whose invoice is missing — are skipped, never
    /// fatal: hydration must not fail startup on one bad row.
    /// The database has no `subscription_id` column on invoices
    /// and no currency on payments / credit notes, so hydrated
    /// invoices carry no subscription link and hydrated payments
    /// / credit notes take the parent invoice's currency.
    /// Call once from `BillingModule::init` after the pool is
    /// attached (see `docs/integration/INTEGRATION_NOTES_C11.md`
    /// for the exact call). Returns the number of rows loaded.
    pub(crate) fn hydrate_from_db(&self, pool: &DbPool) -> UnifiedResult<usize> {
        // The `invoices` table lives at
        // `schema::invoices::invoices`: the file module
        // `schema::invoices` shadows the table name (see the
        // NOTE on the `Invoice` model).
        use unified_core::db::schema::invoices::invoices::dsl as inv_dsl;
        use unified_core::db::schema::{
            credit_notes::dsl as cn_dsl, invoice_items::dsl as ii_dsl,
            invoice_payments::dsl as ip_dsl, invoice_sequences::dsl as seq_dsl,
        };
        let mut conn = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot get database connection: {error}"))
        })?;
        let db_invoices = inv_dsl::invoices
            .select(DbInvoice::as_select())
            .load::<DbInvoice>(&mut conn)
            .map_err(|error| UnifiedError::internal(format!("Cannot load invoices: {error}")))?;
        let db_items = ii_dsl::invoice_items
            .select(DbInvoiceItem::as_select())
            .load::<DbInvoiceItem>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load invoice items: {error}"))
            })?;
        let db_payments = ip_dsl::invoice_payments
            .select(DbInvoicePayment::as_select())
            .load::<DbInvoicePayment>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load invoice payments: {error}"))
            })?;
        let db_credit_notes = cn_dsl::credit_notes
            .select(DbCreditNote::as_select())
            .load::<DbCreditNote>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load credit notes: {error}"))
            })?;
        let db_sequences = seq_dsl::invoice_sequences
            .select(DbInvoiceSequence::as_select())
            .load::<DbInvoiceSequence>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load invoice sequences: {error}"))
            })?;

        let mut items_by_invoice: std::collections::HashMap<String, Vec<InvoiceItem>> =
            std::collections::HashMap::new();
        // Items need the invoice currency for the NUMERIC ->
        // minor-unit conversion; invoices are converted first
        // and their currencies remembered here.
        let mut currency_by_invoice: std::collections::HashMap<Uuid, String> =
            std::collections::HashMap::new();
        let mut invoices: Vec<Invoice> = Vec::with_capacity(db_invoices.len());
        for row in &db_invoices {
            let currency = row.currency.clone();
            let Some(subtotal) = pg_numeric_to_minor(&row.subtotal, &currency) else {
                continue;
            };
            let Some(tax_total) = pg_numeric_to_minor(&row.tax_total, &currency) else {
                continue;
            };
            let Some(total) = pg_numeric_to_minor(&row.total, &currency) else {
                continue;
            };
            let Some(amount_paid) = pg_numeric_to_minor(&row.amount_paid, &currency) else {
                continue;
            };
            let Some(credit_total) = pg_numeric_to_minor(&row.credit_total, &currency) else {
                continue;
            };
            let Some(balance) = pg_numeric_to_minor(&row.balance, &currency) else {
                continue;
            };
            currency_by_invoice.insert(row.id, currency.clone());
            invoices.push(Invoice {
                id: row.id.to_string(),
                number: row.number.clone(),
                reference: row.reference.clone(),
                order_id: row.order_id.clone(),
                subscription_id: None,
                user_id: row.user_id.clone(),
                company_id: row.company_id.map(|id| id.to_string()),
                items: Vec::new(),
                subtotal,
                tax_total,
                total,
                currency,
                status: row.status.clone(),
                overdue: row.overdue,
                tax_inclusive: row.tax_inclusive,
                amount_paid,
                credit_total,
                balance,
                issue_date: row.issue_date,
                due_date: row.due_date,
                payment_terms_days: row
                    .payment_terms_days
                    .filter(|days| *days >= 0)
                    .map(|days| days as u32),
                paid_at: row.paid_at,
                notes: row.notes.clone(),
                reminders_sent: row
                    .reminders_sent
                    .as_array()
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|value| value.as_i64().map(|n| n as i32))
                            .collect()
                    })
                    .unwrap_or_default(),
                created_at: row.created_at,
                updated_at: row.updated_at,
            });
        }
        for item in &db_items {
            let Some(currency) = currency_by_invoice.get(&item.invoice_id) else {
                continue;
            };
            let Some(unit_price) = pg_numeric_to_minor(&item.unit_price, currency) else {
                continue;
            };
            let Some(line_total) = pg_numeric_to_minor(&item.line_total, currency) else {
                continue;
            };
            let Some(tax_amount) = pg_numeric_to_minor(&item.tax_amount, currency) else {
                continue;
            };
            items_by_invoice
                .entry(item.invoice_id.to_string())
                .or_default()
                .push(InvoiceItem {
                    name: item.name.clone(),
                    quantity: item.quantity.max(0) as u32,
                    unit_price,
                    line_total,
                    tax_rate: item.tax_rate,
                    tax_amount,
                });
        }
        let mut loaded = 0;
        self.invoices.clear();
        for mut invoice in invoices {
            if let Some(items) = items_by_invoice.remove(&invoice.id) {
                invoice.items = items;
            }
            self.invoices.insert(invoice.id.clone(), invoice);
            loaded += 1;
        }
        self.payments.clear();
        for row in &db_payments {
            let Some(currency) = currency_by_invoice.get(&row.invoice_id) else {
                continue;
            };
            let Some(amount) = pg_numeric_to_minor(&row.amount, currency) else {
                continue;
            };
            self.payments.insert(
                row.id.to_string(),
                InvoicePayment {
                    id: row.id.to_string(),
                    invoice_id: row.invoice_id.to_string(),
                    amount,
                    method: row.method.clone(),
                    reference: row.reference.clone(),
                    recorded_at: row.recorded_at,
                },
            );
            loaded += 1;
        }
        self.credit_notes.clear();
        for row in &db_credit_notes {
            let Some(currency) = currency_by_invoice.get(&row.invoice_id) else {
                continue;
            };
            let Some(amount) = pg_numeric_to_minor(&row.amount, currency) else {
                continue;
            };
            self.credit_notes.insert(
                row.id.to_string(),
                CreditNote {
                    id: row.id.to_string(),
                    invoice_id: row.invoice_id.to_string(),
                    number: row.number.clone(),
                    amount,
                    reason: row.reason.clone(),
                    created_at: row.created_at,
                },
            );
            loaded += 1;
        }
        // Reseed the number sequences: the sequences table wins
        // when it has rows; otherwise the largest loaded number
        // (+ 1) is the floor, so a restart never reissues a
        // number that already exists.
        let mut next_invoice = 1u64;
        let mut next_credit = 1u64;
        for sequence in &db_sequences {
            let value = sequence.next_value.max(1) as u64;
            if sequence.name == "invoice" {
                next_invoice = next_invoice.max(value);
            } else if sequence.name == "credit_note" {
                next_credit = next_credit.max(value);
            }
        }
        for invoice in self.invoices.iter() {
            if let Some(seq) = invoice
                .value()
                .number
                .rsplit('-')
                .next()
                .and_then(|tail| tail.parse::<u64>().ok())
            {
                next_invoice = next_invoice.max(seq + 1);
            }
        }
        for note in self.credit_notes.iter() {
            if let Some(seq) = note
                .value()
                .number
                .rsplit('-')
                .next()
                .and_then(|tail| tail.parse::<u64>().ok())
            {
                next_credit = next_credit.max(seq + 1);
            }
        }
        self.invoice_seq.fetch_max(next_invoice, Ordering::SeqCst);
        self.credit_seq.fetch_max(next_credit, Ordering::SeqCst);
        Ok(loaded)
    }

    /// Build a draft invoice from an order payload (as emitted by the
    /// store on `order.created`). Returns `None` when the payload lacks
    /// the fields an invoice needs. Order line amounts are treated as
    /// net amounts; tax is computed from the configured default rate
    /// and tax-inclusive flag.
    pub(crate) fn invoice_from_order_value(&self, order: &Value) -> Option<Invoice> {
        let order_id = order.get("id")?.as_str()?.to_string();
        let user_id = order.get("user_id")?.as_str()?.to_string();
        let settings = self.settings();
        let created = now();
        // Order line totals are gross: the coupon discount lives on
        // the order (`discount_total`), not on the lines. Allocating
        // the discount across the lines in proportion to their gross
        // amounts -- with the rounding remainder on the largest line --
        // keeps the lines reconciled to the order total; without it
        // the lines sum to the pre-coupon subtotal while the amount
        // due is net.
        let currency = order
            .get("currency")
            .and_then(|v| v.as_str())
            .unwrap_or("USD")
            .to_string();
        // Order JSON carries integer minor units (legacy emitters
        // sent major-unit floats, which are still accepted); convert
        // once at this boundary and keep integer minor units below.
        // An out-of-range amount fails the invoice instead of
        // silently saturating.
        let total = value_to_minor(order.get("total")?, &currency)?;
        let discount_total = order
            .get("discount_total")
            .and_then(|value| value_to_minor(value, &currency))
            .unwrap_or(0)
            .max(0);
        let mut raw_lines: Vec<(String, u32, i64, i64)> = Vec::new();
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
                let unit_price = raw
                    .get("unit_price")
                    .and_then(|value| value_to_minor(value, &currency))
                    .unwrap_or(0);
                // Prefer the order's own line total; fall back to unit
                // price times quantity.
                let gross = raw
                    .get("line_total")
                    .and_then(|value| value_to_minor(value, &currency))
                    .unwrap_or(unit_price.saturating_mul(i64::from(quantity)))
                    .max(0);
                raw_lines.push((name, quantity, unit_price, gross));
            }
        }
        let gross_sum: i64 = raw_lines.iter().map(|line| line.3).sum();
        let mut allocations: Vec<i64> = vec![0; raw_lines.len()];
        if discount_total > 0 && gross_sum > 0 {
            let discount = discount_total.min(gross_sum);
            let mut allocated_sum: i64 = 0;
            for (index, line) in raw_lines.iter().enumerate() {
                // Proportional share in integer arithmetic
                // (i128 intermediate): the old float math rounded
                // `discount * line / gross_sum` to cents, which is
                // exactly this rounded division in minor units.
                let share = saturating_i64(div_rounded(
                    i128::from(discount) * i128::from(line.3),
                    i128::from(gross_sum),
                ));
                allocations[index] = share.min(line.3);
                allocated_sum += allocations[index];
            }
            // The rounding remainder goes on the largest line so the
            // allocations sum to exactly the discount.
            let remainder = discount - allocated_sum;
            if remainder != 0 {
                if let Some((largest, _)) = raw_lines
                    .iter()
                    .enumerate()
                    .max_by(|(_, a), (_, b)| a.3.cmp(&b.3))
                {
                    allocations[largest] =
                        (allocations[largest] + remainder).clamp(0, raw_lines[largest].3);
                }
            }
        }
        let mut items: Vec<InvoiceItem> = Vec::new();
        let mut subtotal: i64 = 0;
        let mut tax_total: i64 = 0;
        for (index, (name, quantity, unit_price, gross)) in
            raw_lines.into_iter().enumerate()
        {
            let base_amount = (gross - allocations[index]).max(0);
            let (net, tax) = line_tax_on_amount(
                base_amount,
                settings.default_tax_rate,
                settings.tax_inclusive,
            );
            let line_total = if settings.tax_inclusive {
                net + tax
            } else {
                net
            };
            subtotal += net;
            tax_total += tax;
            items.push(InvoiceItem {
                name,
                quantity,
                unit_price,
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
                net + tax
            } else {
                net
            };
            subtotal += net;
            tax_total += tax;
            items.push(InvoiceItem {
                name: "Order".to_string(),
                quantity: 1,
                unit_price: total,
                line_total,
                tax_rate: settings.default_tax_rate,
                tax_amount: tax,
            });
        }
        let computed_total = subtotal + tax_total;
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
            subtotal,
            tax_total,
            total: computed_total,
            currency,
            status: "draft".to_string(),
            overdue: false,
            tax_inclusive: settings.tax_inclusive,
            amount_paid: 0,
            credit_total: 0,
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
    /// subscription payload -- either the subscription object itself
    /// (as emitted by `subscription.created`) or an event payload
    /// carrying it under `"subscription"`. Commercial fields are
    /// always overwritten from the store record; lifecycle
    /// bookkeeping (renewal invoice, reminder, dunning counters)
    /// survives the sync, except that a new period clears the
    /// per-period bookkeeping. Returns `None` when the payload
    /// lacks the fields a mirror needs.
    pub(crate) fn sync_subscription_lifecycle(&self, value: &Value) -> Option<SubscriptionLifecycle> {
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
                .and_then(|value| {
                    value_to_minor(
                        value,
                        subscription
                            .get("currency")
                            .and_then(|value| value.as_str())
                            .unwrap_or("USD"),
                    )
                })
                .unwrap_or(0),
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
            net + tax
        } else {
            net
        };
        let total = net + tax;
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
                unit_price: mirror.price,
                line_total,
                tax_rate: settings.default_tax_rate,
                tax_amount: tax,
            }],
            subtotal: net,
            tax_total: tax,
            total,
            currency: mirror.currency.clone(),
            status: "issued".to_string(),
            overdue: false,
            tax_inclusive: settings.tax_inclusive,
            amount_paid: 0,
            credit_total: 0,
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
    pub(crate) fn build_proration_invoice(
        &self,
        mirror: &SubscriptionLifecycle,
        previous_product_id: &str,
        credit: i64,
        charge: i64,
    ) -> Invoice {
        let settings = self.settings();
        let created = now();
        let mut items: Vec<InvoiceItem> = Vec::new();
        if charge > 0 {
            items.push(InvoiceItem {
                name: format!("Plan change charge — {}", mirror.product_id),
                quantity: 1,
                unit_price: charge,
                line_total: charge,
                tax_rate: 0.0,
                tax_amount: 0,
            });
        }
        if credit > 0 {
            items.push(InvoiceItem {
                name: format!("Unused time credit — {previous_product_id}"),
                quantity: 1,
                unit_price: -credit,
                line_total: -credit,
                tax_rate: 0.0,
                tax_amount: 0,
            });
        }
        let net = charge - credit;
        let total = net.max(0);
        let nothing_due = net <= 0;
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
            tax_total: 0,
            total,
            currency: mirror.currency.clone(),
            status: if nothing_due {
                "paid".to_string()
            } else {
                "issued".to_string()
            },
            overdue: false,
            tax_inclusive: settings.tax_inclusive,
            amount_paid: 0,
            credit_total: 0,
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
    pub(crate) fn request_notification(&self, user_id: &str, title: &str, body: &str, kind: &str) {
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
    pub(crate) fn request_renewal_charge(
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
                "amount": minor_to_major(mirror.price, &mirror.currency),
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
    pub(crate) fn maybe_renew_subscription(&self, invoice: &Invoice) {
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
        if invoice.status != "paid" || invoice.balance > 0 {
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
    /// Billing amounts are integer minor units; each one is
    /// converted through its 4-decimal major-unit string form so
    /// the pricing module parses it with integer math. The invoice carries
    /// no separate discount or proration records (order coupons
    /// are allocated across the lines in `invoice_from_order_value`
    /// and proration credits travel as negative lines), so the
    /// `discounts` and `proration_lines` arrays are empty here by
    /// construction, not by omission. A caller-supplied total that
    /// differs from the computed one fails reconciliation by
    /// design: that is the guard working.
    pub(crate) fn validate_invoice_with_pricing(
        &self,
        tenant_id: &str,
        invoice: &Invoice,
    ) -> UnifiedResult<()> {
        let line = |item: &InvoiceItem| {
            json!({
                "name": item.name,
                "quantity": item.quantity,
                // 4-decimal string form of the amount; the
                // pricing module parses it as scaled integer units.
                "unit_price": format!("{:.4}", minor_to_major(item.unit_price, &invoice.currency)),
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
                    "subtotal": format!("{:.4}", minor_to_major(invoice.subtotal, &invoice.currency)),
                    "discounts": "0.0000",
                    "proration": "0.0000",
                    "tax_total": format!("{:.4}", minor_to_major(invoice.tax_total, &invoice.currency)),
                    "total": format!("{:.4}", minor_to_major(invoice.total, &invoice.currency)),
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
    /// validation failure -- no pricing answer, a mismatch, or no
    /// tenant id on the mirror to validate against -- the invoice
    /// is returned as a `draft` with the reason in its notes, so
    /// the caller must not request a charge for it; the next scan
    /// (or a manual issue, which validates with the request's
    /// tenant context) retries it. The tenant id here is the
    /// mirror's company id, which is the tenant id in the
    /// single-company deployment shape; when it is absent the
    /// invoice cannot be validated and is held as a draft.
    pub(crate) fn build_validated_renewal_invoice(
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
    pub(crate) fn validate_proration_invoice(
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
    pub(crate) fn run_subscription_scan(&self, scope: &Option<String>) -> Value {
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
            // A scoped trigger only scans its own company's
            // subscriptions; the scheduler hook passes `&None` and
            // keeps the global scan.
            if !in_company_scope(&mirror.company_id, scope) {
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
                    if mirror.next_retry_at.is_none() {
                        // A mirror that became past-due through a
                        // sync without ever failing has no retry
                        // scheduled yet: book the first retry at
                        // now plus the retry interval, so dunning
                        // does not stall forever waiting for a
                        // `next_retry_at` that never comes.
                        let first_retry = at
                            + Duration::days(i64::from(
                                mirror.dunning_retry_interval_days,
                            ));
                        if let Some(mut stored) =
                            self.subscription_lifecycles.get_mut(id)
                        {
                            stored.next_retry_at = Some(first_retry);
                            stored.updated_at = at;
                        }
                        details.push(json!({
                            "subscription_id": mirror.subscription_id,
                            "action": "dunning_retry_scheduled",
                            "next_retry_at": first_retry,
                        }));
                    } else if let Some(next_retry) = mirror.next_retry_at {
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
            // ends -- plus the charge request for it. The invoice
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

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::billing::school_fees::{build_fee_invoice, sync_fee_charge_from_invoice};
    use crate::modules::billing::school_fees_charges::existing_fee_receipt_for_amount;

    fn test_mirror(status: &str) -> SubscriptionLifecycle {
        let at = now();
        SubscriptionLifecycle {
            subscription_id: "sub-1".to_string(),
            user_id: "user-1".to_string(),
            company_id: Some("co-1".to_string()),
            product_id: "prod-1".to_string(),
            module_id: None,
            order_id: "ord-1".to_string(),
            status: status.to_string(),
            billing_period: "monthly".to_string(),
            price: 1000,
            currency: "USD".to_string(),
            trial_days: 0,
            trial_end: None,
            current_period_start: at - Duration::days(31),
            current_period_end: at - Duration::days(1),
            proration_policy: "prorate".to_string(),
            grace_period_days: 5,
            dunning_max_retries: 3,
            dunning_retry_interval_days: 3,
            cancel_at_period_end: false,
            cancelled_at: None,
            renewal_invoice_id: Some("inv-1".to_string()),
            renewal_invoice_for_period_end: None,
            renewal_reminder_sent_for: None,
            dunning_attempts: 0,
            last_failure_at: None,
            next_retry_at: None,
            created_at: at,
            updated_at: at,
        }
    }

    fn test_invoice_for_sync(status: &str, total: i64, amount_paid: i64) -> Invoice {
        Invoice {
            id: "inv-1".to_string(),
            number: "INV-000001".to_string(),
            reference: String::new(),
            order_id: None,
            subscription_id: None,
            user_id: "stu-1".to_string(),
            company_id: None,
            items: Vec::new(),
            subtotal: total,
            tax_total: 0,
            total,
            currency: "IDR".to_string(),
            status: status.to_string(),
            overdue: false,
            tax_inclusive: false,
            amount_paid,
            credit_total: total - amount_paid,
            balance: 0,
            issue_date: None,
            due_date: None,
            payment_terms_days: None,
            paid_at: Some(now()),
            notes: None,
            reminders_sent: Vec::new(),
            created_at: now(),
            updated_at: now(),
        }
    }

    fn test_fee_charge(status: &str, total_minor: i64, paid_minor: i64) -> FeeCharge {
        FeeCharge {
            id: "chg-1".to_string(),
            schedule_id: "sch-1".to_string(),
            student_user_id: "stu-1".to_string(),
            fee_type_id: "ft-1".to_string(),
            academic_period_id: "2026-11".to_string(),
            period_label: "2026-11".to_string(),
            amount_minor: total_minor,
            discount_minor: 0,
            total_minor,
            amount_paid_minor: paid_minor,
            currency: "IDR".to_string(),
            invoice_id: Some("inv-1".to_string()),
            discount_ids: Vec::new(),
            due_date: now(),
            status: status.to_string(),
            overdue: false,
            paid_at: Some(now()),
            created_at: now(),
            updated_at: now(),
            company_id: Some("co-1".to_string()),
        }
    }

    fn test_fee_type() -> SchoolFeeType {
        SchoolFeeType {
            id: "ft-1".to_string(),
            code: "tuition".to_string(),
            name: "Tuition".to_string(),
            description: None,
            category: "tuition".to_string(),
            billing_period: "monthly".to_string(),
            default_amount_minor: 10000,
            currency: "IDR".to_string(),
            auto_assign_on_enrolment: false,
            active: true,
            created_at: now(),
            updated_at: now(),
        }
    }

    #[test]
    fn scan_schedules_first_retry_for_past_due_without_one() {
        let state = Arc::new(BillingState::new());
        state
            .subscription_lifecycles
            .insert("sub-1".to_string(), test_mirror("past_due"));
        let before = now();
        let summary = state.run_subscription_scan(&None);
        let after = now();
        assert_eq!(summary["dunning_retries_requested"].as_u64(), Some(0));
        let stored = state
            .subscription_lifecycles
            .get("sub-1")
            .map(|entry| entry.value().clone())
            .expect("mirror still present");
        let retry = stored.next_retry_at.expect("first retry scheduled");
        assert!(retry >= before + Duration::days(3));
        assert!(retry <= after + Duration::days(3));
    }

    #[test]
    fn scan_leaves_an_existing_retry_schedule_alone() {
        let state = Arc::new(BillingState::new());
        let mut mirror = test_mirror("past_due");
        let scheduled = now() + Duration::days(2);
        mirror.next_retry_at = Some(scheduled);
        state
            .subscription_lifecycles
            .insert("sub-1".to_string(), mirror);
        state.run_subscription_scan(&None);
        let stored = state
            .subscription_lifecycles
            .get("sub-1")
            .map(|entry| entry.value().clone())
            .expect("mirror still present");
        assert_eq!(stored.next_retry_at, Some(scheduled));
    }

    #[test]
    fn scan_does_not_schedule_a_retry_when_retries_are_exhausted() {
        let state = Arc::new(BillingState::new());
        let mut mirror = test_mirror("past_due");
        mirror.dunning_attempts = 3;
        state
            .subscription_lifecycles
            .insert("sub-1".to_string(), mirror);
        state.run_subscription_scan(&None);
        let stored = state
            .subscription_lifecycles
            .get("sub-1")
            .map(|entry| entry.value().clone())
            .expect("mirror still present");
        assert_eq!(stored.next_retry_at, None);
    }

    #[test]
    fn build_fee_invoice_is_held_as_draft_without_a_pricing_answer() {
        let state = Arc::new(BillingState::new());
        let charge = test_fee_charge("open", 10000, 0);
        let (invoice, validation) =
            build_fee_invoice(&state, &charge, &test_fee_type(), "2026-11");
        assert!(validation.is_err());
        assert_eq!(invoice.status, "draft");
        assert_eq!(invoice.company_id.as_deref(), Some("co-1"));
        assert!(state.invoices.contains_key(invoice.id.as_str()));
    }

    #[test]
    fn build_fee_invoice_is_held_as_draft_without_a_tenant_id() {
        let state = Arc::new(BillingState::new());
        let mut charge = test_fee_charge("open", 10000, 0);
        charge.company_id = None;
        let (invoice, validation) =
            build_fee_invoice(&state, &charge, &test_fee_type(), "2026-11");
        assert!(validation.is_err());
        assert_eq!(invoice.status, "draft");
        assert!(invoice
            .notes
            .as_deref()
            .unwrap_or("")
            .contains("tenant"));
    }

    #[test]
    fn sync_fee_charge_reopens_after_a_full_refund() {
        let state = Arc::new(BillingState::new());
        state.invoices.insert(
            "inv-1".to_string(),
            test_invoice_for_sync("credited", 10000, 0),
        );
        state.fee_charges.insert(
            "chg-1".to_string(),
            test_fee_charge("paid", 10000, 10000),
        );
        let invoice = state
            .invoices
            .get("inv-1")
            .map(|entry| entry.value().clone())
            .expect("invoice present");
        let (updated, _, _) = sync_fee_charge_from_invoice(&state, &invoice);
        let charge = updated.expect("charge synced");
        assert_eq!(charge.status, "open");
        assert_eq!(charge.amount_paid_minor, 0);
        assert_eq!(charge.paid_at, None);
    }

    #[test]
    fn sync_fee_charge_reopens_as_partially_paid_after_a_partial_refund() {
        let state = Arc::new(BillingState::new());
        state.invoices.insert(
            "inv-1".to_string(),
            test_invoice_for_sync("credited", 10000, 4000),
        );
        state.fee_charges.insert(
            "chg-1".to_string(),
            test_fee_charge("paid", 10000, 10000),
        );
        let invoice = state
            .invoices
            .get("inv-1")
            .map(|entry| entry.value().clone())
            .expect("invoice present");
        let (updated, _, _) = sync_fee_charge_from_invoice(&state, &invoice);
        let charge = updated.expect("charge synced");
        assert_eq!(charge.status, "partially_paid");
        assert_eq!(charge.amount_paid_minor, 4000);
    }

    #[test]
    fn existing_fee_receipt_matches_by_charge_and_amount() {
        let state = Arc::new(BillingState::new());
        state.fee_receipts.insert(
            "rcpt-1".to_string(),
            FeeReceipt {
                id: "rcpt-1".to_string(),
                number: "RCPT-000001".to_string(),
                charge_id: "chg-1".to_string(),
                student_user_id: "stu-1".to_string(),
                amount_minor: 5000,
                currency: "IDR".to_string(),
                method: None,
                reference: None,
                issued_by: "mgr-1".to_string(),
                issued_at: now(),
            },
        );
        assert!(existing_fee_receipt_for_amount(&state, "chg-1", 5000));
        assert!(!existing_fee_receipt_for_amount(&state, "chg-1", 3000));
        assert!(!existing_fee_receipt_for_amount(&state, "chg-2", 5000));
    }
}
