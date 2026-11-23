//! payments - limits.

use std::time::Duration;
use super::{events::*, card_validation::*, card_settings::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperswitch::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



// ---------------------------------------------------------------------------
// Limits.
// ---------------------------------------------------------------------------
/// Default HTTP timeout for provider API calls.
pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Default connect timeout for provider API calls.
pub(crate) const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 10;

/// Longest `Retry-After` delay honoured from a provider.
pub(crate) const MAX_RETRY_AFTER: Duration = Duration::from_secs(120);

/// User-Agent sent to provider APIs.
pub(crate) const USER_AGENT: &str = concat!("unified/", env!("CARGO_PKG_VERSION"), " payments");

/// Maximum amount in minor units accepted for one charge (sanity guard).
pub(crate) const MAX_AMOUNT_MINOR: i64 = 1_000_000_000_000;

/// Webhook event ids are remembered for this long (duplicate suppression).
pub(crate) const WEBHOOK_EVENT_TTL_SECS: i64 = 86_400;


/// Maximum age of a Stripe webhook signature timestamp, in seconds.
/// Stripe signs `timestamp.body`; without a window a captured signed
/// payload stays replayable forever, so timestamps older than this
/// are rejected even when the signature itself verifies.
pub(crate) const STRIPE_WEBHOOK_TOLERANCE_SECS: i64 = 300;
