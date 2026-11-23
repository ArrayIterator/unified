//! payments - providers.

use serde::{Deserialize, Serialize};
use super::{events::*, card_validation::*, card_settings::*, limits::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperpay::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
// ---------------------------------------------------------------------------
// Provider kinds, capabilities, currencies, settings fields.
// ---------------------------------------------------------------------------
/// Every payment provider this module can talk to.
pub enum PaymentProviderKind {
    Xendit,
    HyperPay,
    TwoCheckout,
    Card,
    Mastercard,
    GooglePay,
    ApplePay,
    PayPal,
    Stripe,
    Midtrans,
    Custom,
    BankTransfer,
}


impl PaymentProviderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Xendit => "xendit",
            Self::HyperPay => "hyperpay",
            Self::TwoCheckout => "2checkout",
            Self::Card => "card",
            Self::Mastercard => "mastercard",
            Self::GooglePay => "google_pay",
            Self::ApplePay => "apple_pay",
            Self::PayPal => "paypal",
            Self::Stripe => "stripe",
            Self::Midtrans => "midtrans",
            Self::Custom => "custom",
            Self::BankTransfer => "bank_transfer",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "xendit" => Some(Self::Xendit),
            "hyperpay" => Some(Self::HyperPay),
            "2checkout" | "2co" | "twocheckout" => Some(Self::TwoCheckout),
            "card" => Some(Self::Card),
            "mastercard" => Some(Self::Mastercard),
            "google_pay" | "googlepay" => Some(Self::GooglePay),
            "apple_pay" | "applepay" => Some(Self::ApplePay),
            "paypal" => Some(Self::PayPal),
            "stripe" => Some(Self::Stripe),
            "midtrans" => Some(Self::Midtrans),
            "custom" => Some(Self::Custom),
            "bank_transfer" | "banktransfer" => Some(Self::BankTransfer),
            _ => None,
        }
    }

    /// All kinds, in a stable order for the catalog endpoint.
    pub fn all() -> &'static [PaymentProviderKind] {
        &[
            Self::Xendit,
            Self::HyperPay,
            Self::TwoCheckout,
            Self::Card,
            Self::Mastercard,
            Self::GooglePay,
            Self::ApplePay,
            Self::PayPal,
            Self::Stripe,
            Self::Midtrans,
            Self::Custom,
            Self::BankTransfer,
        ]
    }

    /// Human-readable display name.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Xendit => "Xendit",
            Self::HyperPay => "HyperPay",
            Self::TwoCheckout => "2Checkout",
            Self::Card => "Credit / Debit Card",
            Self::Mastercard => "Mastercard Gateway",
            Self::GooglePay => "Google Pay",
            Self::ApplePay => "Apple Pay",
            Self::PayPal => "PayPal",
            Self::Stripe => "Stripe",
            Self::Midtrans => "Midtrans",
            Self::Custom => "Custom Provider",
            Self::BankTransfer => "Bank Transfer (Manual Verification)",
        }
    }

    /// Hosts this provider's API is allowed to talk to. A request to any
    /// other host is refused before it is sent. Empty means the host is
    /// validated by the adapter instead (per-region hosts, or a
    /// user-supplied endpoint).
    pub fn allowed_hosts(&self) -> &'static [&'static str] {
        match self {
            Self::Xendit => &["api.xendit.co"],
            Self::HyperPay => &["eu-test.oppwa.com", "eu-prod.oppwa.com"],
            Self::TwoCheckout => &["api.2checkout.com"],
            Self::Card => &[],
            // User-supplied processor endpoint; validated as an https
            // URL with a host and no embedded credentials.
            Self::Mastercard => &[],
            // Merchant gateway host (`<gateway>.mastercard.com`),
            // validated by `MastercardAdapter::validate_endpoint`.
            Self::GooglePay | Self::ApplePay => &[],
            // No direct API: the delegate connection's hosts apply.
            Self::PayPal => &["api-m.paypal.com", "api-m.sandbox.paypal.com"],
            Self::Stripe => &["api.stripe.com"],
            Self::Midtrans => &["api.midtrans.com", "api.sandbox.midtrans.com"],
            Self::Custom => &[],
            // User-supplied API base URL; validated as an https URL
            // with a host and no embedded credentials.
            Self::BankTransfer => &[],
            // No provider API exists: verification is manual.
        }
    }

    /// Whether the API host is fixed (allow-listed) or user-supplied.
    pub fn has_fixed_hosts(&self) -> bool {
        matches!(
            self,
            Self::Xendit | Self::HyperPay | Self::TwoCheckout | Self::PayPal | Self::Stripe | Self::Midtrans
        )
    }

    /// Whether the provider needs a delegate gateway connection to
    /// perform charges (wallet adapters only).
    pub fn needs_delegate(&self) -> bool {
        matches!(self, Self::GooglePay | Self::ApplePay)
    }

    /// Whether the provider needs a user-supplied endpoint on the
    /// connection.
    pub fn needs_endpoint(&self) -> bool {
        matches!(self, Self::Card | Self::Mastercard | Self::Custom)
    }
}


/// What a provider adapter can do. Callers must check these flags
/// before calling an operation: an unsupported call fails with a 400
/// naming the missing capability.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct PaymentCapabilities {
    pub supports_authorize: bool,
    pub supports_capture: bool,
    pub supports_charge: bool,
    pub supports_refund: bool,
    pub supports_void: bool,
    pub supports_subscriptions: bool,
    pub supports_payouts: bool,
    pub supports_3ds: bool,
    pub supports_webhooks: bool,
    /// True when subscriptions are a local schedule over tokenized
    /// charges rather than a native provider subscription object.
    pub subscriptions_are_local_schedule: bool,
}


/// Capabilities per kind (single source of truth for the catalog, the
/// read views, and the operation guards). The `subscriptions_are_local
/// _schedule` flag marks providers where the subscription record lives
/// in this module and each period is a tokenized charge, because the
/// provider exposes no subscription object to this adapter.
pub fn capabilities_for(kind: PaymentProviderKind) -> PaymentCapabilities {
    match kind {
        PaymentProviderKind::Xendit => PaymentCapabilities {
            supports_authorize: true,
            supports_capture: true,
            supports_charge: true,
            supports_refund: true,
            supports_void: false,
            supports_subscriptions: true,
            supports_payouts: true,
            supports_3ds: true,
            supports_webhooks: true,
            subscriptions_are_local_schedule: true,
        },
        PaymentProviderKind::HyperPay => PaymentCapabilities {
            supports_authorize: true,
            supports_capture: true,
            supports_charge: true,
            supports_refund: true,
            supports_void: true,
            supports_subscriptions: true,
            supports_payouts: false,
            supports_3ds: true,
            supports_webhooks: true,
            subscriptions_are_local_schedule: true,
        },
        PaymentProviderKind::TwoCheckout => PaymentCapabilities {
            supports_authorize: false,
            supports_capture: false,
            supports_charge: true,
            supports_refund: true,
            supports_void: false,
            supports_subscriptions: true,
            supports_payouts: false,
            supports_3ds: true,
            supports_webhooks: true,
            subscriptions_are_local_schedule: true,
        },
        PaymentProviderKind::Card => PaymentCapabilities {
            supports_authorize: true,
            supports_capture: true,
            supports_charge: true,
            supports_refund: true,
            supports_void: true,
            supports_subscriptions: true,
            supports_payouts: false,
            supports_3ds: true,
            supports_webhooks: true,
            subscriptions_are_local_schedule: true,
        },
        PaymentProviderKind::Mastercard => PaymentCapabilities {
            supports_authorize: true,
            supports_capture: true,
            supports_charge: true,
            supports_refund: true,
            supports_void: true,
            supports_subscriptions: true,
            supports_payouts: false,
            supports_3ds: true,
            supports_webhooks: true,
            subscriptions_are_local_schedule: true,
        },
        PaymentProviderKind::GooglePay | PaymentProviderKind::ApplePay => PaymentCapabilities {
            // The wallet forwards its token to the delegate gateway;
            // the effective capabilities are the delegate's. The flags
            // below are the wallet's own envelope: charge and refund
            // through the delegate, no native authorize / payout /
            // subscription object of its own.
            supports_authorize: false,
            supports_capture: false,
            supports_charge: true,
            supports_refund: true,
            supports_void: false,
            supports_subscriptions: false,
            supports_payouts: false,
            supports_3ds: false,
            supports_webhooks: false,
            subscriptions_are_local_schedule: false,
        },
        PaymentProviderKind::PayPal => PaymentCapabilities {
            supports_authorize: true,
            supports_capture: true,
            supports_charge: true,
            supports_refund: true,
            supports_void: true,
            supports_subscriptions: true,
            supports_payouts: true,
            supports_3ds: true,
            supports_webhooks: true,
            subscriptions_are_local_schedule: false,
        },
        PaymentProviderKind::Stripe => PaymentCapabilities {
            supports_authorize: true,
            supports_capture: true,
            supports_charge: true,
            supports_refund: true,
            supports_void: true,
            supports_subscriptions: true,
            supports_payouts: true,
            supports_3ds: true,
            supports_webhooks: true,
            subscriptions_are_local_schedule: false,
        },
        PaymentProviderKind::Midtrans => PaymentCapabilities {
            supports_authorize: true,
            supports_capture: true,
            supports_charge: true,
            supports_refund: true,
            supports_void: false,
            supports_subscriptions: true,
            supports_payouts: false,
            supports_3ds: true,
            supports_webhooks: true,
            subscriptions_are_local_schedule: false,
        },
        PaymentProviderKind::Custom => PaymentCapabilities {
            supports_authorize: true,
            supports_capture: true,
            supports_charge: true,
            supports_refund: true,
            supports_void: true,
            supports_subscriptions: true,
            supports_payouts: true,
            supports_3ds: false,
            supports_webhooks: true,
            subscriptions_are_local_schedule: false,
        },
        // Bank transfer: there is no provider API. A charge or an
        // authorization only opens a pending manual verification
        // (handled by the bank-transfer module); the administrator
        // approval is the capture and emits `payment.captured`, so
        // this adapter reports no provider-side capture. Refund and
        // void are manual operations: the adapter records the request
        // and the administrator moves the money and keeps the audit
        // note. No subscriptions, payouts, 3-D Secure, or webhooks.
        PaymentProviderKind::BankTransfer => PaymentCapabilities {
            supports_authorize: true,
            supports_capture: false,
            supports_charge: true,
            supports_refund: true,
            supports_void: true,
            supports_subscriptions: false,
            supports_payouts: false,
            supports_3ds: false,
            supports_webhooks: false,
            subscriptions_are_local_schedule: false,
        },
    }
}


/// Currencies the adapter can charge in. These are the currencies the
/// provider documents for the API surface used here; a charge in any
/// other currency is rejected before any provider call. Wallet kinds
/// use the delegate connection's list at charge time; the catalog
/// shows the common wallet list.
pub fn currencies_for(kind: PaymentProviderKind) -> &'static [&'static str] {
    match kind {
        PaymentProviderKind::Xendit => &["IDR", "PHP", "THB", "VND", "MYR", "SGD", "USD"],
        PaymentProviderKind::HyperPay => &["AED", "SAR", "USD", "EUR", "GBP", "EGP", "JOD", "KWD", "QAR", "BHD", "OMR"],
        PaymentProviderKind::TwoCheckout => &["USD", "EUR", "GBP", "AUD", "CAD", "JPY", "CHF", "SEK", "NOK", "DKK", "PLN", "CZK", "HUF", "BRL", "MXN", "INR", "ZAR", "ILS"],
        PaymentProviderKind::Card => &["USD", "EUR", "GBP", "IDR", "SGD", "MYR", "PHP", "THB", "AED", "SAR", "AUD", "CAD", "JPY", "CHF"],
        PaymentProviderKind::Mastercard => &["USD", "EUR", "GBP", "AUD", "CAD", "JPY", "SGD", "HKD", "NZD", "CHF", "SEK", "NOK", "DKK", "AED", "SAR", "INR", "IDR", "MYR", "PHP", "THB", "ZAR", "BRL", "MXN"],
        PaymentProviderKind::GooglePay | PaymentProviderKind::ApplePay => {
            &["USD", "EUR", "GBP", "AUD", "CAD", "JPY", "SGD", "CHF"]
        }
        PaymentProviderKind::PayPal => &["USD", "EUR", "GBP", "AUD", "CAD", "JPY", "SGD", "HKD", "NZD", "CHF", "SEK", "NOK", "DKK", "PLN", "CZK", "HUF", "ILS", "MXN", "BRL", "ZAR", "KRW", "TWD", "THB", "IDR", "MYR", "PHP", "INR"],
        PaymentProviderKind::Stripe => &["USD", "EUR", "GBP", "AUD", "CAD", "JPY", "SGD", "HKD", "NZD", "CHF", "SEK", "NOK", "DKK", "PLN", "CZK", "HUF", "ILS", "MXN", "BRL", "INR", "IDR", "MYR", "PHP", "THB", "VND", "ZAR", "KRW", "TWD", "AED"],
        PaymentProviderKind::Midtrans => &["IDR", "SGD"],
        PaymentProviderKind::Custom => &["USD", "EUR", "GBP", "IDR", "SGD"],
        // Bank accounts are configured per currency by the operator;
        // the adapter accepts the common settlement currencies and
        // the bank-transfer module enforces the account's currency.
        PaymentProviderKind::BankTransfer => &["USD", "EUR", "GBP", "IDR", "SGD", "MYR", "PHP", "THB", "AUD", "CAD", "JPY", "CHF", "AED", "SAR"],
    }
}


/// One field of a provider's settings page: name, type, whether it is
/// required, whether it is a secret (write-only, masked on read), and
/// whether it applies to sandbox, live, or both modes.
#[derive(Debug, Clone, Serialize)]
pub struct SettingField {
    pub name: &'static str,
    pub label: &'static str,
    #[serde(rename = "type")]
    pub field_type: &'static str,
    pub required: bool,
    pub secret: bool,
    pub applies_to: &'static str,
    pub description: &'static str,
}


pub(crate) fn field(
    name: &'static str,
    label: &'static str,
    field_type: &'static str,
    required: bool,
    secret: bool,
    applies_to: &'static str,
    description: &'static str,
) -> SettingField {
    SettingField {
        name,
        label,
        field_type,
        required,
        secret,
        applies_to,
        description,
    }
}


/// Settings-page field definitions per provider kind.
pub fn settings_fields_for(kind: PaymentProviderKind) -> Vec<SettingField> {
    match kind {
        PaymentProviderKind::Xendit => vec![
            field("api_key", "Secret key", "password", true, true, "both", "Xendit secret key (xnd_development_... in sandbox, xnd_production_... in live). Used as the Basic-auth username."),
            field("webhook_secret", "Callback token", "password", true, true, "both", "Xendit callback token (X-Callback-Token). Must match the token configured in the Xendit dashboard."),
            field("public_key", "Public key", "text", false, false, "both", "Xendit public key, only needed when tokenizing cards client-side."),
        ],
        PaymentProviderKind::HyperPay => vec![
            field("access_token", "Access token", "password", true, true, "both", "HyperPay access token (Bearer). Sandbox and live tokens differ; switch mode to use the matching one."),
            field("merchant_id", "Entity ID", "text", true, false, "both", "HyperPay entityId (the channel / entity identifier for checkouts)."),
            field("webhook_secret", "Webhook secret", "password", false, true, "both", "Shared secret for webhook verification when the operator configures HyperPay notifications with one. When empty, webhooks are verified by re-fetching the payment status from HyperPay."),
        ],
        PaymentProviderKind::TwoCheckout => vec![
            field("merchant_id", "Merchant code", "text", true, false, "both", "2Checkout merchant code."),
            field("secret_word", "Secret word", "password", true, true, "both", "2Checkout secret word, used to sign REST API requests (HMAC-SHA256)."),
            field("api_key", "API key", "password", false, true, "both", "Optional 2Checkout API key when the account uses key authentication instead of the secret word."),
        ],
        PaymentProviderKind::Card => vec![
            field("api_key", "Processor API key", "password", true, true, "both", "API key for the card processor behind the endpoint. Sent as a Bearer token unless the processor expects an X-Api-Key header."),
            field("webhook_secret", "Webhook secret", "password", false, true, "both", "Shared secret for HMAC-SHA256 webhook verification (X-Signature header)."),
            field("merchant_id", "Merchant ID", "text", false, false, "both", "Merchant identifier sent to the processor when it requires one."),
        ],
        PaymentProviderKind::Mastercard => vec![
            field("merchant_id", "Merchant ID", "text", true, false, "both", "Mastercard gateway merchant id (used as the Basic-auth username, prefixed with merchant.)."),
            field("secret_key", "API password", "password", true, true, "both", "Mastercard gateway API password (Basic-auth password)."),
            field("webhook_secret", "Webhook secret", "password", false, true, "both", "Shared secret for HMAC-SHA256 webhook verification (X-Webhook-Signature header)."),
        ],
        PaymentProviderKind::GooglePay => vec![
            field("merchant_id", "Google Pay merchant ID", "text", false, false, "both", "Google Pay merchant id, for reference on the connection. Charges run through the delegate gateway below."),
            field("delegate_connection_id", "Delegate gateway connection", "connection", true, false, "both", "Id of the gateway connection (Stripe, Xendit, Mastercard, ...) that performs the charge for a Google Pay token."),
        ],
        PaymentProviderKind::ApplePay => vec![
            field("merchant_id", "Apple Pay merchant ID", "text", false, false, "both", "Apple Pay merchant id, for reference on the connection. Charges run through the delegate gateway below."),
            field("delegate_connection_id", "Delegate gateway connection", "connection", true, false, "both", "Id of the gateway connection (Stripe, Xendit, Mastercard, ...) that performs the charge for an Apple Pay token."),
        ],
        PaymentProviderKind::PayPal => vec![
            field("client_id", "Client ID", "text", true, false, "both", "PayPal REST app client id."),
            field("client_secret", "Client secret", "password", true, true, "both", "PayPal REST app client secret. Used only for the OAuth2 token exchange; never returned."),
            field("webhook_id", "Webhook ID", "text", true, false, "both", "PayPal webhook id (from the PayPal dashboard), required to verify webhook signatures server-side."),
        ],
        PaymentProviderKind::Stripe => vec![
            field("secret_key", "Secret key", "password", true, true, "both", "Stripe secret key (sk_test_... in sandbox, sk_live_... in live)."),
            field("public_key", "Publishable key", "text", false, false, "both", "Stripe publishable key (pk_test_... / pk_live_...), only needed for client-side tokenization."),
            field("webhook_secret", "Webhook secret", "password", true, true, "both", "Stripe webhook signing secret (whsec_...), used to verify the Stripe-Signature header."),
        ],
        PaymentProviderKind::Midtrans => vec![
            field("server_key", "Server key", "password", true, true, "both", "Midtrans server key (SB-Mid-server-... in sandbox, Mid-server-... in live). Used for Basic auth and webhook signature verification."),
            field("client_key", "Client key", "text", false, false, "both", "Midtrans client key, only needed for client-side tokenization (Snap / Core API)."),
            field("merchant_id", "Merchant ID", "text", false, false, "both", "Midtrans merchant id, for reference on the connection."),
        ],
        PaymentProviderKind::Custom => vec![
            field("api_key", "API key", "password", false, true, "both", "API key sent according to the custom auth type (Bearer, header, or query)."),
            field("secret_key", "Secret / signing key", "password", false, true, "both", "Secret used for HMAC-SHA256 request signing and webhook verification when the auth type is hmac_sha256."),
            field("username", "Username", "text", false, false, "both", "Username for Basic auth when the auth type is basic."),
            field("password", "Password", "password", false, true, "both", "Password for Basic auth when the auth type is basic."),
            field("webhook_secret", "Webhook secret", "password", false, true, "both", "Shared secret for HMAC-SHA256 webhook verification when the auth type is hmac_sha256."),
        ],
        // Bank transfer has no API credentials and no endpoint. These
        // fields document the verification knobs; the live values are
        // the bank-transfer module settings (GET / PUT
        // /api/v1/bank-transfer/settings).
        PaymentProviderKind::BankTransfer => vec![
            field("amount_tolerance_minor", "Amount tolerance (minor units)", "text", false, false, "both", "Maximum minor-unit difference accepted between the transferred amount and the order total before the proof is flagged as a mismatch."),
            field("proof_expiry_days", "Proof expiry (days)", "text", false, false, "both", "Days after which an unsubmitted proof request expires."),
        ],
    }
}
