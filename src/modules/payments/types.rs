//! payments - types.

use unified_core::reference_id;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperpay::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
// ---------------------------------------------------------------------------
// Shared result shapes.
// ---------------------------------------------------------------------------
/// Normalized payment status, shared by every adapter.
pub enum PaymentStatus {
    RequiresConfirmation,
    Authorized,
    Captured,
    Pending,
    Failed,
    Refunded,
    PartiallyRefunded,
    Voided,
    Chargeback,
    Cancelled,
}


impl PaymentStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RequiresConfirmation => "requires_confirmation",
            Self::Authorized => "authorized",
            Self::Captured => "captured",
            Self::Pending => "pending",
            Self::Failed => "failed",
            Self::Refunded => "refunded",
            Self::PartiallyRefunded => "partially_refunded",
            Self::Voided => "voided",
            Self::Chargeback => "chargeback",
            Self::Cancelled => "cancelled",
        }
    }
}


/// What a charge / authorize / capture call returns, in provider-neutral
/// form. `fee_minor` is set only when the provider reports a fee.
#[derive(Debug, Clone, Serialize)]
pub struct ChargeResult {
    pub provider_transaction_id: String,
    pub status: PaymentStatus,
    pub amount_minor: i64,
    pub currency: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fee_minor: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_url: Option<String>,
    pub requires_action: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}


/// What a refund call returns.
#[derive(Debug, Clone, Serialize)]
pub struct RefundResult {
    pub provider_refund_id: String,
    pub status: PaymentStatus,
    pub amount_minor: i64,
    pub currency: String,
}


/// What a subscription create call returns.
#[derive(Debug, Clone, Serialize)]
pub struct SubscriptionResult {
    pub provider_subscription_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_period_end: Option<DateTime<Utc>>,
}


/// What a payout call returns.
#[derive(Debug, Clone, Serialize)]
pub struct PayoutResult {
    pub provider_payout_id: String,
    pub status: PaymentStatus,
    pub amount_minor: i64,
    pub currency: String,
}


/// Result of a connection test.
#[derive(Debug, Clone, Serialize)]
pub struct ConnectionTestResult {
    pub kind: String,
    pub mode: String,
    pub reachable: bool,
    pub latency_ms: u64,
    pub detail: String,
}



#[derive(Debug, Clone, Default)]
// ---------------------------------------------------------------------------
// Requests (adapter input).
// ---------------------------------------------------------------------------
/// Billing details supplied by the shopper. Only 2Checkout requires
/// them (an order cannot be placed without billing details); every
/// other provider ignores them.
pub struct BillingDetails {
    pub name: String,
    pub email: String,
    pub country: String,
    pub city: Option<String>,
    pub address: Option<String>,
}


/// Input to an adapter charge / authorize call. Amounts are integer
/// minor units; the payment method is always a provider token, never a
/// card number.
#[derive(Debug, Clone)]
pub struct ChargeInput {
    pub amount_minor: i64,
    pub currency: String,
    pub customer_id: String,
    pub order_id: Option<String>,
    /// Provider-facing payment reference for this charge, in the
    /// shared 30-character reference format (see
    /// `unified_core::reference_id`). Threaded from the store order's
    /// reference when the caller knows it; adapters derive one from
    /// `order_id` or generate a fresh one when this is `None`.
    /// Never a card number or other sensitive value.
    pub reference_id: Option<String>,
    pub invoice_id: Option<String>,
    pub description: Option<String>,
    /// Tokenized payment method (card token, wallet token, vault id).
    pub payment_method_token: Option<String>,
    pub idempotency_key: String,
    /// True when the caller wants a 3-D Secure challenge when the
    /// provider requires one.
    pub three_ds: bool,
    /// Billing details, required by 2Checkout, ignored elsewhere.
    pub billing: Option<BillingDetails>,
}


// Batch 18: payment reference ids (END)

#[derive(Debug, Clone)]
// ---------------------------------------------------------------------------
/// Input to an adapter refund call.
pub struct RefundInput {
    pub provider_transaction_id: String,
    pub amount_minor: Option<i64>,
    pub currency: String,
    pub reason: Option<String>,
    pub idempotency_key: String,
}


/// Input to an adapter subscription create call.
#[derive(Debug, Clone)]
pub struct SubscriptionInput {
    pub customer_id: String,
    /// Provider price / plan id (Stripe price id, PayPal plan id,
    /// Midtrans subscription name). Ignored by local-schedule
    /// providers, which charge `amount_minor` per period instead.
    pub plan_id: Option<String>,
    pub amount_minor: i64,
    pub currency: String,
    /// `day`, `week`, `month`, or `year`.
    pub interval: String,
    pub interval_count: u32,
    pub trial_days: u32,
    pub payment_method_token: Option<String>,
    pub order_id: Option<String>,
    pub idempotency_key: String,
}


/// Input to an adapter payout call.
#[derive(Debug, Clone)]
pub struct PayoutInput {
    pub amount_minor: i64,
    pub currency: String,
    /// Destination account identifier (bank account number, PayPal
    /// email, Stripe connected account id, depending on the provider).
    pub destination_account: String,
    /// Provider-specific destination qualifier (bank code for Xendit
    /// disbursements, payout method for PayPal).
    pub destination_type: Option<String>,
    pub description: Option<String>,
    pub idempotency_key: String,
}
