// Diesel models for the pos domain of the unified-db-commerce bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// Point of sale (migration 000076_pos).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = pos_registers)]
pub struct PosRegister {
    pub id: Uuid,
    pub name: String,
    pub code: String,
    pub location: Option<String>,
    pub status: String,
    pub cashiers: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = pos_registers)]
pub struct NewPosRegister {
    pub id: Uuid,
    pub name: String,
    pub code: String,
    pub location: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pos_shifts)]
pub struct PosShift {
    pub id: Uuid,
    pub register_id: Uuid,
    pub cashier_id: String,
    pub status: String,
    pub opening_float: PgNumeric,
    pub currency: String,
    pub expected_cash: Option<PgNumeric>,
    pub counted_cash: Option<PgNumeric>,
    pub variance: Option<PgNumeric>,
    pub notes: Option<String>,
    pub opened_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pos_shifts)]
pub struct NewPosShift {
    pub id: Uuid,
    pub register_id: Uuid,
    pub cashier_id: String,
    pub expected_cash: Option<PgNumeric>,
    pub counted_cash: Option<PgNumeric>,
    pub variance: Option<PgNumeric>,
    pub notes: Option<String>,
    pub closed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pos_sales)]
pub struct PosSale {
    pub id: Uuid,
    pub number: String,
    pub register_id: Uuid,
    pub shift_id: Option<Uuid>,
    pub cashier_id: String,
    pub company_id: Option<String>,
    pub discount_type: Option<String>,
    pub discount_value: PgNumeric,
    pub discount_total: PgNumeric,
    pub subtotal: PgNumeric,
    pub tax_total: PgNumeric,
    pub total: PgNumeric,
    pub currency: String,
    pub tax_inclusive: bool,
    pub status: String,
    pub amount_paid: PgNumeric,
    pub change_due: PgNumeric,
    pub refunded_amount: PgNumeric,
    pub customer_name: Option<String>,
    pub notes: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub voided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pos_sales)]
pub struct NewPosSale {
    pub id: Uuid,
    pub number: String,
    pub register_id: Uuid,
    pub shift_id: Option<Uuid>,
    pub cashier_id: String,
    pub company_id: Option<String>,
    pub discount_type: Option<String>,
    pub customer_name: Option<String>,
    pub notes: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub voided_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pos_sale_items)]
pub struct PosSaleItem {
    pub id: Uuid,
    pub sale_id: Uuid,
    pub product_id: String,
    pub name: String,
    pub barcode: Option<String>,
    pub sku: Option<String>,
    pub unit_price: PgNumeric,
    pub quantity: i32,
    pub refunded_quantity: i32,
    pub discount_type: Option<String>,
    pub discount_value: PgNumeric,
    pub discount_amount: PgNumeric,
    pub line_subtotal: PgNumeric,
    pub tax_rate: PgNumeric,
    pub tax_amount: PgNumeric,
    pub line_total: PgNumeric,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pos_sale_items)]
pub struct NewPosSaleItem {
    pub id: Uuid,
    pub sale_id: Uuid,
    pub product_id: String,
    pub name: String,
    pub barcode: Option<String>,
    pub sku: Option<String>,
    pub discount_type: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pos_payments)]
pub struct PosPayment {
    pub id: Uuid,
    pub sale_id: Uuid,
    pub method_id: String,
    pub method_code: String,
    pub amount: PgNumeric,
    pub tendered: PgNumeric,
    pub reference: Option<String>,
    pub recorded_by: String,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pos_payments)]
pub struct NewPosPayment {
    pub id: Uuid,
    pub sale_id: Uuid,
    pub method_id: String,
    pub method_code: String,
    pub amount: PgNumeric,
    pub tendered: PgNumeric,
    pub reference: Option<String>,
    pub recorded_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pos_refunds)]
pub struct PosRefund {
    pub id: Uuid,
    pub sale_id: Uuid,
    pub shift_id: Option<Uuid>,
    pub amount: PgNumeric,
    pub method_id: String,
    pub method_code: String,
    pub items: Value,
    pub reason: String,
    pub refunded_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pos_refunds)]
pub struct NewPosRefund {
    pub id: Uuid,
    pub sale_id: Uuid,
    pub shift_id: Option<Uuid>,
    pub amount: PgNumeric,
    pub method_id: String,
    pub method_code: String,
    pub reason: String,
    pub refunded_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = pos_receipts)]
pub struct PosReceipt {
    pub id: Uuid,
    pub number: String,
    pub sale_id: Uuid,
    pub sale_number: String,
    pub register_id: Uuid,
    pub register_code: Option<String>,
    pub cashier_id: String,
    pub store_name: String,
    pub store_address: Option<String>,
    pub subtotal: PgNumeric,
    pub discount_total: PgNumeric,
    pub tax_total: PgNumeric,
    pub total: PgNumeric,
    pub currency: String,
    pub tax_breakdown: Value,
    pub amount_paid: PgNumeric,
    pub change_due: PgNumeric,
    pub print_lines: Value,
    pub footer: Option<String>,
    pub issued_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = pos_receipts)]
pub struct NewPosReceipt {
    pub id: Uuid,
    pub number: String,
    pub sale_id: Uuid,
    pub sale_number: String,
    pub register_id: Uuid,
    pub register_code: Option<String>,
    pub cashier_id: String,
    pub store_name: String,
    pub store_address: Option<String>,
    pub footer: Option<String>,
}
