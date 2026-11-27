//! payments - requests.

use chrono::{DateTime, Utc};
use serde::{Deserialize};
use uuid::Uuid;
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperswitch::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



#[derive(Debug, Clone, Deserialize)]
// API request types.
pub struct CreateConnectionRequest {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub credentials: PaymentCredentials,
    #[serde(default)]
    pub custom_config: Option<CustomGatewayConfig>,
    #[serde(default)]
    pub delegate_connection_id: Option<Uuid>,
    #[serde(default)]
    pub enabled: Option<bool>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct UpdateConnectionRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub credentials: Option<PaymentCredentials>,
    #[serde(default)]
    pub custom_config: Option<CustomGatewayConfig>,
    #[serde(default)]
    pub delegate_connection_id: Option<Uuid>,
    #[serde(default)]
    pub enabled: Option<bool>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct SwitchModeRequest {
    pub mode: String,
}


#[derive(Debug, Clone, Deserialize)]
pub struct BillingDetailsRequest {
    pub name: String,
    pub email: String,
    pub country: String,
    #[serde(default)]
    pub city: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct CreateIntentRequest {
    pub connection_id: Uuid,
    pub customer_id: String,
    #[serde(default)]
    pub order_id: Option<String>,
    #[serde(default)]
    pub invoice_id: Option<String>,
    pub amount_minor: i64,
    pub currency: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub payment_method_token: Option<String>,
    #[serde(default)]
    pub payment_method_id: Option<Uuid>,
    #[serde(default)]
    pub capture_method: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub billing: Option<BillingDetailsRequest>,
    #[serde(default)]
    pub three_ds: bool,
    /// Raw card details for validation only (see `CardDetailsInput`).
    /// When present, confirm / charge runs the card validation gate
    /// before any provider call. Never stored.
    #[serde(default)]
    pub card: Option<CardDetailsInput>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct ConfirmIntentRequest {
    #[serde(default)]
    pub payment_method_token: Option<String>,
    #[serde(default)]
    pub payment_method_id: Option<Uuid>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// Raw card details for validation only (see `CardDetailsInput`).
    /// When present, confirm runs the card validation gate before
    /// any provider call. Never stored.
    #[serde(default)]
    pub card: Option<CardDetailsInput>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct CaptureIntentRequest {
    #[serde(default)]
    pub amount_minor: Option<i64>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct CreateChargeRequest {
    pub connection_id: Uuid,
    pub customer_id: String,
    #[serde(default)]
    pub order_id: Option<String>,
    #[serde(default)]
    pub invoice_id: Option<String>,
    pub amount_minor: i64,
    pub currency: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub payment_method_token: Option<String>,
    #[serde(default)]
    pub payment_method_id: Option<Uuid>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub billing: Option<BillingDetailsRequest>,
    #[serde(default)]
    pub three_ds: bool,
    /// Raw card details for validation only (see `CardDetailsInput`).
    /// When present, the charge runs the card validation gate
    /// before any provider call. Never stored.
    #[serde(default)]
    pub card: Option<CardDetailsInput>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct CreateRefundRequest {
    #[serde(default)]
    pub amount_minor: Option<i64>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}


/// Complete a pending refund: `status` defaults to `refunded`
/// and accepts `refunded` or `failed`.
#[derive(Debug, Clone, Deserialize)]
pub struct CompleteRefundRequest {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct CreatePayoutRequest {
    pub connection_id: Uuid,
    pub amount_minor: i64,
    pub currency: String,
    pub destination_account: String,
    #[serde(default)]
    pub destination_type: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct CreatePaymentMethodRequest {
    pub customer_id: String,
    pub connection_id: Uuid,
    pub provider_token: String,
    pub brand: String,
    pub last4: String,
    pub exp_month: u32,
    pub exp_year: u32,
    #[serde(default)]
    pub is_default: bool,
    /// Raw card details for validation only (see `CardDetailsInput`).
    /// When present, the method is validated before it is stored
    /// and the stored brand comes from the validator's answer.
    /// Never stored.
    #[serde(default)]
    pub card: Option<CardDetailsInput>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct CreateSubscriptionRequest {
    pub connection_id: Uuid,
    pub customer_id: String,
    #[serde(default)]
    pub plan_id: Option<String>,
    pub amount_minor: i64,
    pub currency: String,
    pub interval: String,
    #[serde(default = "default_interval_count")]
    pub interval_count: u32,
    #[serde(default)]
    pub trial_days: u32,
    #[serde(default)]
    pub payment_method_id: Option<Uuid>,
    #[serde(default)]
    pub payment_method_token: Option<String>,
    #[serde(default)]
    pub order_id: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// Dunning settings for this subscription, copied from the
    /// product/plan when the caller knows them (the products
    /// module stores `grace_period_days`, `dunning_max_retries`,
    /// and `dunning_retry_interval_days` per product). When
    /// absent, the product defaults apply (grace 0 days, 3
    /// retries, 3 days between retries). The renewal code reads
    /// these stored values; without them a failed renewal would
    /// retry forever.
    #[serde(default)]
    pub grace_period_days: Option<u32>,
    #[serde(default)]
    pub dunning_max_retries: Option<u32>,
    #[serde(default)]
    pub dunning_retry_interval_days: Option<u32>,
    /// Raw card details for validation only (see `CardDetailsInput`).
    /// When present, subscription creation runs the card validation
    /// gate before any provider call. Never stored.
    #[serde(default)]
    pub card: Option<CardDetailsInput>,
}


/// Product defaults for the dunning settings, mirroring the
/// products module (`default_dunning_max_retries`,
/// `default_dunning_retry_interval_days`, grace 0).
pub(crate) fn default_dunning_max_retries() -> u32 {
    3
}


pub(crate) fn default_dunning_retry_interval_days() -> u32 {
    3
}


pub(crate) fn default_grace_period_days() -> u32 {
    0
}


pub(crate) fn default_interval_count() -> u32 {
    1
}


#[derive(Debug, Clone, Deserialize)]
pub struct CancelSubscriptionRequest {
    #[serde(default)]
    pub reason: Option<String>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct PayOrderRequest {
    pub connection_id: Uuid,
    pub amount_minor: i64,
    pub currency: String,
    pub customer_id: String,
    #[serde(default)]
    pub invoice_id: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub payment_method_token: Option<String>,
    #[serde(default)]
    pub payment_method_id: Option<Uuid>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub billing: Option<BillingDetailsRequest>,
    #[serde(default)]
    pub three_ds: bool,
    /// Raw card details for validation only (see `CardDetailsInput`).
    /// When present, the order payment runs the card validation
    /// gate before any provider call. Never stored.
    #[serde(default)]
    pub card: Option<CardDetailsInput>,
}



#[derive(Debug, Clone, Deserialize)]
// Split bill request types.
pub struct SplitPartInput {
    pub amount_minor: i64,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub connection_id: Option<Uuid>,
    #[serde(default)]
    pub payer_id: Option<String>,
    #[serde(default)]
    pub due_date: Option<DateTime<Utc>>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct CreateSplitRequest {
    #[serde(default)]
    pub order_id: Option<String>,
    #[serde(default)]
    pub invoice_id: Option<String>,
    pub customer_id: String,
    pub total_minor: i64,
    pub currency: String,
    pub parts: Vec<SplitPartInput>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct PaySplitPartRequest {
    #[serde(default)]
    pub connection_id: Option<Uuid>,
    #[serde(default)]
    pub customer_id: Option<String>,
    #[serde(default)]
    pub payment_method_token: Option<String>,
    #[serde(default)]
    pub payment_method_id: Option<Uuid>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub three_ds: bool,
    /// Raw card details for validation only (see `CardDetailsInput`).
    /// When present, the split-part payment runs the card
    /// validation gate before any provider call. Never stored.
    #[serde(default)]
    pub card: Option<CardDetailsInput>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct CancelSplitRequest {
    #[serde(default)]
    pub reason: Option<String>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct GenerateScheduleRequest {
    #[serde(default)]
    pub order_id: Option<String>,
    #[serde(default)]
    pub invoice_id: Option<String>,
    pub customer_id: String,
    pub total_minor: i64,
    pub currency: String,
    pub parts_count: u32,
    pub first_due_date: DateTime<Utc>,
    #[serde(default)]
    pub interval_days: Option<u32>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub payer_id: Option<String>,
    #[serde(default)]
    pub connection_id: Option<Uuid>,
    #[serde(default)]
    pub amounts: Option<Vec<i64>>,
    #[serde(default)]
    pub create: bool,
}
