// Diesel models for the procurement domain of the unified-db-commerce bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, NaiveDate, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Procurement (migration 000049_procurement).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = vendors)]
pub struct Vendor {
    pub id: Uuid,
    pub name: String,
    pub contact_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub address: Option<String>,
    pub categories: Vec<String>,
    pub tags: Vec<String>,
    pub status: String,
    pub payment_reference: Option<String>,
    pub notes: Option<String>,
    pub rating: Option<i16>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = vendors)]
pub struct NewVendor {
    pub id: Uuid,
    pub name: String,
    pub contact_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub address: Option<String>,
    pub payment_reference: Option<String>,
    pub notes: Option<String>,
    pub rating: Option<i16>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = purchase_orders)]
pub struct PurchaseOrder {
    pub id: Uuid,
    pub po_number: String,
    pub vendor_id: Uuid,
    pub title: String,
    pub category: Option<String>,
    pub currency: String,
    pub status: String,
    pub subtotal: PgNumeric,
    pub tax_total: PgNumeric,
    pub total: PgNumeric,
    pub promised_date: Option<NaiveDate>,
    pub ordered_at: Option<DateTime<Utc>>,
    pub received_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
    pub requested_by: String,
    pub approved_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = purchase_orders)]
pub struct NewPurchaseOrder {
    pub id: Uuid,
    pub po_number: String,
    pub vendor_id: Uuid,
    pub title: String,
    pub category: Option<String>,
    pub promised_date: Option<NaiveDate>,
    pub ordered_at: Option<DateTime<Utc>>,
    pub received_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
    pub requested_by: String,
    pub approved_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = purchase_order_lines)]
pub struct PurchaseOrderLine {
    pub id: Uuid,
    pub purchase_order_id: Uuid,
    pub description: String,
    pub quantity: PgNumeric,
    pub unit_price: PgNumeric,
    pub tax_rate: PgNumeric,
    pub line_total: PgNumeric,
    pub received_quantity: PgNumeric,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = purchase_order_lines)]
pub struct NewPurchaseOrderLine {
    pub id: Uuid,
    pub purchase_order_id: Uuid,
    pub description: String,
    pub quantity: PgNumeric,
    pub unit_price: PgNumeric,
    pub line_total: PgNumeric,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = purchase_order_approvals)]
pub struct PurchaseOrderApproval {
    pub id: Uuid,
    pub purchase_order_id: Uuid,
    pub approver_id: String,
    pub approver_role: String,
    pub decision: String,
    pub reason: Option<String>,
    pub decided_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = purchase_order_approvals)]
pub struct NewPurchaseOrderApproval {
    pub id: Uuid,
    pub purchase_order_id: Uuid,
    pub approver_id: String,
    pub approver_role: String,
    pub decision: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = goods_receipts)]
pub struct GoodsReceipt {
    pub id: Uuid,
    pub purchase_order_id: Uuid,
    pub line_id: Uuid,
    pub quantity_received: PgNumeric,
    pub quality_note: Option<String>,
    pub received_by: String,
    pub received_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = goods_receipts)]
pub struct NewGoodsReceipt {
    pub id: Uuid,
    pub purchase_order_id: Uuid,
    pub line_id: Uuid,
    pub quantity_received: PgNumeric,
    pub quality_note: Option<String>,
    pub received_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = procurement_budgets)]
pub struct ProcurementBudget {
    pub id: Uuid,
    pub category: Option<String>,
    pub vendor_id: Option<Uuid>,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub budget_amount: PgNumeric,
    pub currency: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = procurement_budgets)]
pub struct NewProcurementBudget {
    pub id: Uuid,
    pub category: Option<String>,
    pub vendor_id: Option<Uuid>,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub budget_amount: PgNumeric,
    pub created_by: String,
}
