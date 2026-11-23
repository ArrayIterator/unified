//! payments - events.

use super::{card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperswitch::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



// ---------------------------------------------------------------------------
// Events.
// ---------------------------------------------------------------------------
/// Emitted after a successful authorization (funds held, not captured).
pub const PAYMENT_AUTHORIZED: &str = "payment.authorized";

/// Emitted after a successful capture / charge.
pub const PAYMENT_CAPTURED: &str = "payment.captured";

/// Emitted after a successful refund.
pub const PAYMENT_REFUNDED: &str = "payment.refunded";

/// Emitted when a charge, authorization, or renewal fails.
pub const PAYMENT_FAILED: &str = "payment.failed";

/// Emitted when a gateway subscription is created.
pub const PAYMENT_SUBSCRIPTION_CREATED: &str = "payment.subscription.created";

/// Emitted when a gateway subscription is cancelled.
pub const PAYMENT_SUBSCRIPTION_CANCELLED: &str = "payment.subscription.cancelled";

/// Emitted when a gateway subscription renews successfully.
pub const PAYMENT_SUBSCRIPTION_RENEWED: &str = "payment.subscription.renewed";

/// Emitted for every verified webhook, before type-specific handling.
pub const PAYMENT_WEBHOOK_RECEIVED: &str = "payment.webhook.received";

/// Emitted when a chargeback webhook arrives.
pub const PAYMENT_CHARGEBACK: &str = "payment.chargeback";

/// Emitted when a split bill is created.
pub const PAYMENT_SPLIT_CREATED: &str = "payment.split.created";

/// Emitted when one part of a split bill is paid.
pub const PAYMENT_SPLIT_PART_PAID: &str = "payment.split.part.paid";

/// Emitted when every part of a split bill is paid.
pub const PAYMENT_SPLIT_COMPLETED: &str = "payment.split.completed";

/// Emitted when a split bill is cancelled.
pub const PAYMENT_SPLIT_CANCELLED: &str = "payment.split.cancelled";
