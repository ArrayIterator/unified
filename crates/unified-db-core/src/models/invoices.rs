// Diesel models for the invoices domain of the unified-db-core bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Queryable, Selectable)]
// NOTE: the path must stay fully qualified to the TABLE module.
// `crate::schema::invoices` is the schema file-module (schema/invoices.rs),
// not the table: the `pub use invoices::*;` re-export in schema.rs
// cannot surface the inner `invoices` table over the file-module of
// the same name, so the table's columns live at
// `crate::schema::invoices::invoices`. Pointing `table_name` at the
// file-module (or the bare name `invoices`) makes every field fail
// with E0425 "cannot find type ... in module `invoices`".
#[diesel(table_name = crate::schema::invoices::invoices)]
pub struct Invoice {
    pub id: Uuid,
    pub number: String,
    pub order_id: Option<String>,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub subtotal: PgNumeric,
    pub tax_total: PgNumeric,
    pub total: PgNumeric,
    pub currency: String,
    pub status: String,
    pub overdue: bool,
    pub tax_inclusive: bool,
    pub amount_paid: PgNumeric,
    pub credit_total: PgNumeric,
    pub balance: PgNumeric,
    pub issue_date: Option<DateTime<Utc>>,
    pub due_date: Option<DateTime<Utc>>,
    pub payment_terms_days: Option<i32>,
    pub paid_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub reminders_sent: Value,
    pub reference: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = crate::schema::invoices::invoices)]
pub struct NewInvoice {
    pub id: Uuid,
    pub number: String,
    pub order_id: Option<String>,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub total: PgNumeric,
    pub currency: String,
    pub status: String,
    pub issue_date: Option<DateTime<Utc>>,
    pub due_date: Option<DateTime<Utc>>,
    pub payment_terms_days: Option<i32>,
    pub paid_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub reference: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = invoice_items)]
pub struct InvoiceItem {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub name: String,
    pub quantity: i32,
    pub unit_price: PgNumeric,
    pub line_total: PgNumeric,
    pub tax_rate: f64,
    pub tax_amount: PgNumeric,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = invoice_items)]
pub struct NewInvoiceItem {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub name: String,
    pub quantity: i32,
    pub unit_price: PgNumeric,
    pub line_total: PgNumeric,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = invoice_payments)]
pub struct InvoicePayment {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub amount: PgNumeric,
    pub method: String,
    pub reference: Option<String>,
    pub recorded_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = invoice_payments)]
pub struct NewInvoicePayment {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub amount: PgNumeric,
    pub method: String,
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = credit_notes)]
pub struct CreditNote {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub number: String,
    pub amount: PgNumeric,
    pub reason: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = credit_notes)]
pub struct NewCreditNote {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub number: String,
    pub amount: PgNumeric,
}
