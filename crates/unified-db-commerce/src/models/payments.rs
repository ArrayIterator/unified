// Diesel models for the payments domain of the unified-db-commerce bounded context.
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

// ---------------------------------------------------------------------------
// Discount coupons.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = store_coupons)]
pub struct StoreCoupon {
    pub id: Uuid,
    pub code: String,
    pub discount_type: String,
    pub discount_value: PgNumeric,
    pub expires_at: Option<DateTime<Utc>>,
    pub usage_limit: Option<i32>,
    pub used_count: i32,
    pub product_ids: Value,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = store_coupons)]
pub struct NewStoreCoupon {
    pub id: Uuid,
    pub code: String,
    pub discount_type: String,
    pub discount_value: PgNumeric,
    pub expires_at: Option<DateTime<Utc>>,
    pub usage_limit: Option<i32>,
    pub used_count: i32,
    pub product_ids: Value,
    pub active: bool,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = billing_settings)]
pub struct BillingSettings {
    pub key: String,
    pub invoice_prefix: String,
    pub default_tax_rate: f64,
    pub tax_inclusive: bool,
    pub payment_terms_days: i32,
    pub reminder_before_days: Value,
    pub reminder_after_days: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = billing_settings)]
pub struct NewBillingSettings {
    pub key: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = invoice_sequences)]
pub struct InvoiceSequence {
    pub name: String,
    pub next_value: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = invoice_sequences)]
pub struct NewInvoiceSequence {
    pub name: String,
}

// ---------------------------------------------------------------------------
// Payments and bank transfer (2026-11-22-000014_payments_bank_transfer).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = payment_connections)]
pub struct PaymentConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub mode: String,
    pub endpoint: Option<String>,
    #[serde(skip_serializing)]
    pub credentials: Value,
    pub custom_config: Option<Value>,
    pub delegate_connection_id: Option<Uuid>,
    pub enabled: bool,
    pub last_tested_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = payment_connections)]
pub struct NewPaymentConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub endpoint: Option<String>,
    pub custom_config: Option<Value>,
    pub delegate_connection_id: Option<Uuid>,
    pub last_tested_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = payment_intents)]
pub struct PaymentIntent {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub provider: String,
    pub customer_id: String,
    pub order_id: Option<String>,
    pub invoice_id: Option<String>,
    pub amount_minor: i64,
    pub currency: String,
    pub status: String,
    pub capture_method: String,
    pub provider_transaction_id: Option<String>,
    pub fee_minor: Option<i64>,
    pub idempotency_key: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = payment_intents)]
pub struct NewPaymentIntent {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub provider: String,
    pub customer_id: String,
    pub order_id: Option<String>,
    pub invoice_id: Option<String>,
    pub amount_minor: i64,
    pub currency: String,
    pub status: String,
    pub capture_method: String,
    pub provider_transaction_id: Option<String>,
    pub fee_minor: Option<i64>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = payment_transactions)]
pub struct PaymentTransaction {
    pub id: Uuid,
    pub intent_id: Option<Uuid>,
    pub connection_id: Uuid,
    pub provider: String,
    pub kind: String,
    pub customer_id: String,
    pub order_id: Option<String>,
    pub invoice_id: Option<String>,
    pub subscription_id: Option<Uuid>,
    pub amount_minor: i64,
    pub currency: String,
    pub fee_minor: Option<i64>,
    pub status: String,
    pub provider_transaction_id: String,
    pub provider_refund_id: Option<String>,
    pub idempotency_key: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = payment_transactions)]
pub struct NewPaymentTransaction {
    pub id: Uuid,
    pub intent_id: Option<Uuid>,
    pub connection_id: Uuid,
    pub provider: String,
    pub kind: String,
    pub customer_id: String,
    pub order_id: Option<String>,
    pub invoice_id: Option<String>,
    pub subscription_id: Option<Uuid>,
    pub amount_minor: i64,
    pub currency: String,
    pub fee_minor: Option<i64>,
    pub status: String,
    pub provider_transaction_id: String,
    pub provider_refund_id: Option<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = payment_methods)]
pub struct PaymentMethod {
    pub id: Uuid,
    pub customer_id: String,
    pub connection_id: Uuid,
    pub provider: String,
    #[serde(skip_serializing)]
    pub provider_token: String,
    pub brand: String,
    pub last4: String,
    pub exp_month: i32,
    pub exp_year: i32,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = payment_methods)]
pub struct NewPaymentMethod {
    pub id: Uuid,
    pub customer_id: String,
    pub connection_id: Uuid,
    pub provider: String,
    pub provider_token: String,
    pub brand: String,
    pub last4: String,
    pub exp_month: i32,
    pub exp_year: i32,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = gateway_subscriptions)]
pub struct GatewaySubscription {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub provider: String,
    pub customer_id: String,
    pub order_id: Option<String>,
    pub plan_id: Option<String>,
    pub amount_minor: i64,
    pub currency: String,
    pub interval: String,
    pub interval_count: i32,
    pub status: String,
    pub provider_subscription_id: String,
    pub payment_method_id: Option<Uuid>,
    pub current_period_end: Option<DateTime<Utc>>,
    pub next_renewal_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = gateway_subscriptions)]
pub struct NewGatewaySubscription {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub provider: String,
    pub customer_id: String,
    pub order_id: Option<String>,
    pub plan_id: Option<String>,
    pub amount_minor: i64,
    pub currency: String,
    pub interval: String,
    pub status: String,
    pub provider_subscription_id: String,
    pub payment_method_id: Option<Uuid>,
    pub current_period_end: Option<DateTime<Utc>>,
    pub next_renewal_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = split_bills)]
pub struct SplitBill {
    pub id: Uuid,
    pub order_id: Option<String>,
    pub invoice_id: Option<String>,
    pub customer_id: String,
    pub total_minor: i64,
    pub currency: String,
    pub status: String,
    pub paid_minor: i64,
    pub remaining_minor: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = split_bills)]
pub struct NewSplitBill {
    pub id: Uuid,
    pub order_id: Option<String>,
    pub invoice_id: Option<String>,
    pub customer_id: String,
    pub total_minor: i64,
    pub currency: String,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = split_parts)]
pub struct SplitPart {
    pub id: Uuid,
    pub split_id: Uuid,
    pub index: i32,
    pub amount_minor: i64,
    pub currency: String,
    pub provider: String,
    pub connection_id: Option<Uuid>,
    pub payer_id: String,
    pub due_date: Option<DateTime<Utc>>,
    pub status: String,
    pub paid_transaction_id: Option<Uuid>,
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = split_parts)]
pub struct NewSplitPart {
    pub id: Uuid,
    pub split_id: Uuid,
    pub index: i32,
    pub amount_minor: i64,
    pub currency: String,
    pub provider: String,
    pub connection_id: Option<Uuid>,
    pub payer_id: String,
    pub due_date: Option<DateTime<Utc>>,
    pub status: String,
    pub paid_transaction_id: Option<Uuid>,
    pub paid_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = bank_accounts)]
pub struct BankAccount {
    pub id: Uuid,
    pub bank_name: String,
    pub account_holder: String,
    pub account_number: String,
    pub swift_bic: Option<String>,
    pub currency: String,
    pub instructions: Option<String>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = bank_accounts)]
pub struct NewBankAccount {
    pub id: Uuid,
    pub bank_name: String,
    pub account_holder: String,
    pub account_number: String,
    pub swift_bic: Option<String>,
    pub currency: String,
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = bank_transfer_verifications)]
pub struct BankTransferVerification {
    pub id: Uuid,
    pub order_id: String,
    pub invoice_id: Option<String>,
    pub customer_id: String,
    pub split_id: Option<Uuid>,
    pub part_id: Option<Uuid>,
    pub bank_account_id: Option<Uuid>,
    pub status: String,
    pub expected_amount_minor: Option<i64>,
    pub currency: String,
    pub proof: Option<Value>,
    pub amount_check: String,
    pub amount_difference_minor: Option<i64>,
    pub transaction_id: Option<String>,
    pub verified_by: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
    pub rejection_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = bank_transfer_verifications)]
pub struct NewBankTransferVerification {
    pub id: Uuid,
    pub order_id: String,
    pub invoice_id: Option<String>,
    pub customer_id: String,
    pub split_id: Option<Uuid>,
    pub part_id: Option<Uuid>,
    pub bank_account_id: Option<Uuid>,
    pub status: String,
    pub expected_amount_minor: Option<i64>,
    pub currency: String,
    pub proof: Option<Value>,
    pub amount_difference_minor: Option<i64>,
    pub transaction_id: Option<String>,
    pub verified_by: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
    pub rejection_reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = bank_transfer_verification_audit)]
pub struct BankTransferVerificationAudit {
    pub id: Uuid,
    pub verification_id: Uuid,
    pub action: String,
    pub actor: String,
    pub actor_role: String,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = bank_transfer_verification_audit)]
pub struct NewBankTransferVerificationAudit {
    pub id: Uuid,
    pub verification_id: Uuid,
    pub action: String,
    pub actor: String,
    pub actor_role: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = bank_transfer_settings)]
pub struct BankTransferSettings {
    pub key: String,
    pub amount_tolerance_minor: i64,
    pub proof_expiry_days: i32,
    pub require_reference_number: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = bank_transfer_settings)]
pub struct NewBankTransferSettings {
    pub key: String,
}
