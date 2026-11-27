// Diesel models for the expenses domain of the unified-db-commerce bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, NaiveDate, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Expenses (migration 000088_expenses).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = expense_categories)]
pub struct ExpenseCategory {
    pub id: Uuid,
    pub name: String,
    pub code: String,
    pub company_id: Option<String>,
    pub receipt_required: bool,
    pub receipt_required_above_minor: Option<i64>,
    pub per_expense_limit_minor: Option<i64>,
    pub per_day_limit_minor: Option<i64>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = expense_categories)]
pub struct NewExpenseCategory {
    pub id: Uuid,
    pub name: String,
    pub code: String,
    pub company_id: Option<String>,
    pub receipt_required_above_minor: Option<i64>,
    pub per_expense_limit_minor: Option<i64>,
    pub per_day_limit_minor: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = expense_policies)]
pub struct ExpensePolicy {
    pub id: Uuid,
    pub name: String,
    pub company_id: Option<String>,
    pub currency: String,
    pub mileage_rate_minor: Option<i64>,
    pub mileage_unit: String,
    pub per_diem_rate_minor: Option<i64>,
    pub max_report_total_minor: Option<i64>,
    pub approval_chain_id: Option<String>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = expense_policies)]
pub struct NewExpensePolicy {
    pub id: Uuid,
    pub name: String,
    pub company_id: Option<String>,
    pub mileage_rate_minor: Option<i64>,
    pub per_diem_rate_minor: Option<i64>,
    pub max_report_total_minor: Option<i64>,
    pub approval_chain_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = expense_reports)]
pub struct ExpenseReport {
    pub id: Uuid,
    pub employee_id: String,
    pub company_id: Option<String>,
    pub policy_id: Option<Uuid>,
    pub title: String,
    pub notes: Option<String>,
    pub status: String,
    pub currency: String,
    pub total_minor: i64,
    pub violations: Vec<String>,
    pub approval_request_id: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decided_by: Option<String>,
    pub decision_reason: Option<String>,
    pub reimbursement_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = expense_reports)]
pub struct NewExpenseReport {
    pub id: Uuid,
    pub employee_id: String,
    pub company_id: Option<String>,
    pub policy_id: Option<Uuid>,
    pub title: String,
    pub notes: Option<String>,
    pub approval_request_id: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decided_by: Option<String>,
    pub decision_reason: Option<String>,
    pub reimbursement_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = expenses)]
pub struct Expense {
    pub id: Uuid,
    pub report_id: Uuid,
    pub kind: String,
    pub category_id: Uuid,
    pub expense_date: NaiveDate,
    pub amount_minor: i64,
    pub currency: String,
    pub description: String,
    pub project_id: Option<String>,
    pub client_id: Option<String>,
    pub distance: Option<f64>,
    pub days: Option<i64>,
    pub rate_minor_applied: Option<i64>,
    pub violations: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = expenses)]
pub struct NewExpense {
    pub id: Uuid,
    pub report_id: Uuid,
    pub kind: String,
    pub category_id: Uuid,
    pub expense_date: NaiveDate,
    pub amount_minor: i64,
    pub description: String,
    pub project_id: Option<String>,
    pub client_id: Option<String>,
    pub distance: Option<f64>,
    pub days: Option<i64>,
    pub rate_minor_applied: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = expense_receipts)]
pub struct ExpenseReceipt {
    pub id: Uuid,
    pub expense_id: Uuid,
    pub storage_target: String,
    pub storage_key: String,
    pub file_name: String,
    pub content_type: Option<String>,
    pub uploaded_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = expense_receipts)]
pub struct NewExpenseReceipt {
    pub id: Uuid,
    pub expense_id: Uuid,
    pub storage_target: String,
    pub storage_key: String,
    pub file_name: String,
    pub content_type: Option<String>,
    pub uploaded_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = expense_reimbursements)]
pub struct ExpenseReimbursement {
    pub id: Uuid,
    pub report_id: Uuid,
    pub employee_id: String,
    pub amount_minor: i64,
    pub currency: String,
    pub status: String,
    pub payable_reference: String,
    pub failure_reason: Option<String>,
    pub paid_at: Option<DateTime<Utc>>,
    pub attempts: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = expense_reimbursements)]
pub struct NewExpenseReimbursement {
    pub id: Uuid,
    pub report_id: Uuid,
    pub employee_id: String,
    pub amount_minor: i64,
    pub payable_reference: String,
    pub failure_reason: Option<String>,
    pub paid_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = expense_adjustments)]
pub struct ExpenseAdjustment {
    pub id: Uuid,
    pub report_id: Uuid,
    pub kind: String,
    pub amount_minor: i64,
    pub currency: String,
    pub reason: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = expense_adjustments)]
pub struct NewExpenseAdjustment {
    pub id: Uuid,
    pub report_id: Uuid,
    pub kind: String,
    pub amount_minor: i64,
    pub reason: String,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = expense_currency_rates)]
pub struct ExpenseCurrencyRate {
    pub id: Uuid,
    pub base_currency: String,
    pub target_currency: String,
    pub rate: f64,
    pub source: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = expense_currency_rates)]
pub struct NewExpenseCurrencyRate {
    pub id: Uuid,
    pub base_currency: String,
    pub target_currency: String,
    pub rate: f64,
}
