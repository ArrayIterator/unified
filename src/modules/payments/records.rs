//! payments - records.

use unified_core::error::{UnifiedError, UnifiedResult};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Serialize};
use serde_json::{Value};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;
use uuid::Uuid;
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperpay::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



#[derive(Debug, Clone, Serialize)]
// ---------------------------------------------------------------------------
// Records: intents, transactions, payment methods, subscriptions.
// ---------------------------------------------------------------------------
/// A payment intent: the record a checkout creates first, then
/// confirms. The intent holds no card data — only a token reference.
pub struct PaymentIntent {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub provider: String,
    pub customer_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invoice_id: Option<String>,
    pub amount_minor: i64,
    pub currency: String,
    /// `requires_confirmation`, `authorized`, `captured`, `cancelled`,
    /// or `failed`.
    pub status: String,
    /// `automatic` (charge on confirm) or `manual` (authorize on
    /// confirm, capture later).
    pub capture_method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_transaction_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fee_minor: Option<i64>,
    /// Tokenized payment method supplied at creation, kept so confirm
    /// can run without the caller resending it. Never serialized: a
    /// token is a payment reference, not a display value.
    #[serde(skip_serializing)]
    pub payment_method_token: Option<String>,
    /// Stored payment method supplied at creation, kept for the
    /// same reason — and so confirm can run the card validation
    /// gate in method mode without the caller resending it.
    /// Never serialized: a method reference, not a display value.
    #[serde(skip_serializing)]
    pub payment_method_id: Option<Uuid>,
    pub idempotency_key: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}


/// One money movement recorded by the gateway: charge, refund, void,
/// payout, or subscription period charge.
#[derive(Debug, Clone, Serialize)]
pub struct PaymentTransaction {
    pub id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent_id: Option<Uuid>,
    pub connection_id: Uuid,
    pub provider: String,
    /// `charge`, `authorize`, `capture`, `refund`, `void`, `payout`,
    /// or `subscription_charge`.
    pub kind: String,
    pub customer_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invoice_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscription_id: Option<Uuid>,
    pub amount_minor: i64,
    pub currency: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fee_minor: Option<i64>,
    pub status: String,
    pub provider_transaction_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_refund_id: Option<String>,
    /// The charge or capture this record reverses, when the record
    /// is a refund. The provider refund id lives in
    /// `provider_transaction_id` / `provider_refund_id` and cannot
    /// be used for this link, so refunds are summed through this
    /// field for the over-refund guard and the completion path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_transaction_id: Option<Uuid>,
    /// Operator (`x-user-id`) that authorized this movement, when
    /// the movement is operator-initiated (payouts). Customer
    /// payments leave this empty: the payer is `customer_id`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator_id: Option<String>,
    pub idempotency_key: String,
    pub created_at: DateTime<Utc>,
}


/// A tokenized payment method for one customer. The record holds the
/// provider token, the card brand, the last four digits, and the
/// expiry — never a PAN or CVV.
#[derive(Debug, Clone, Serialize)]
pub struct PaymentMethod {
    pub id: Uuid,
    pub customer_id: String,
    pub connection_id: Uuid,
    pub provider: String,
    /// Provider token (vault id / card token / wallet token).
    pub provider_token: String,
    pub brand: String,
    pub last4: String,
    pub exp_month: u32,
    pub exp_year: u32,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
}


/// A gateway subscription: native provider subscriptions (Stripe,
/// PayPal, Midtrans) store the provider subscription id; local
/// -schedule providers store the id of the initial charge and renew
/// by charging the stored token each period.
#[derive(Debug, Clone, Serialize)]
pub struct GatewaySubscription {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub provider: String,
    pub customer_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    pub amount_minor: i64,
    pub currency: String,
    pub interval: String,
    pub interval_count: u32,
    /// `pending` (created at the provider but awaiting shopper
    /// approval, for example PayPal), `active`, `past_due`,
    /// `cancelled`, or `ended`.
    pub status: String,
    pub provider_subscription_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment_method_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_period_end: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_renewal_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancelled_at: Option<DateTime<Utc>>,
    /// Dunning settings copied from the product/plan at creation
    /// (see `CreateSubscriptionRequest`): how many consecutive
    /// failed renewals are retried, how many days apart, and how
    /// long access survives after the retries run out. The renewal
    /// code reads these; they are defaults, not decoration.
    pub dunning_max_retries: u32,
    pub dunning_retry_interval_days: u32,
    pub grace_period_days: u32,
    /// Consecutive failed renewal attempts for the current period.
    /// Reset to zero by a captured renewal.
    pub consecutive_failures: u32,
    /// True once the failure count reached `dunning_max_retries`:
    /// no further charge is attempted; the subscription either
    /// waits out its grace period or has already expired.
    pub dunning_exhausted: bool,
    /// End of the grace period after dunning is exhausted. `None`
    /// while dunning is still running (or when the grace period
    /// is zero and exhaustion expires the subscription at once).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grace_ends_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}



#[derive(Debug, Clone, Serialize)]
// ---------------------------------------------------------------------------
// Split bills: one order/invoice total divided across methods,
// payers, or instalments.
// ---------------------------------------------------------------------------
/// One part of a split bill. Amounts are integer minor units; the
/// provider is a registry kind string (`stripe`, `bank_transfer`,
/// ...). A part is paid through the gateway like any other charge,
/// except a `bank_transfer` part, which opens a manual verification
/// in the bank-transfer module instead of charging.
pub struct SplitPart {
    pub id: Uuid,
    pub split_id: Uuid,
    pub index: usize,
    pub amount_minor: i64,
    pub currency: String,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<Uuid>,
    pub payer_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_date: Option<DateTime<Utc>>,
    /// `pending`, `paid`, `failed`, or `cancelled`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paid_transaction_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paid_at: Option<DateTime<Utc>>,
}


/// A split bill: one order/invoice total divided into parts. The
/// split is `active` until every part is paid (`completed`) or it is
/// cancelled. Completion is only possible when the full total is
/// covered: the sum of the parts always equals `total_minor`.
#[derive(Debug, Clone, Serialize)]
pub struct SplitBill {
    pub id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invoice_id: Option<String>,
    pub customer_id: String,
    pub total_minor: i64,
    pub currency: String,
    /// `active`, `completed`, or `cancelled`.
    pub status: String,
    pub parts: Vec<SplitPart>,
    pub paid_minor: i64,
    pub remaining_minor: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}



// ---------------------------------------------------------------------------
// In-memory stores (same pattern as the provider registry and store).
// ---------------------------------------------------------------------------
pub(crate) static CONNECTIONS: OnceLock<DashMap<Uuid, PaymentConnection>> = OnceLock::new();


pub(crate) fn connections_store() -> &'static DashMap<Uuid, PaymentConnection> {
    CONNECTIONS.get_or_init(DashMap::new)
}


pub(crate) static INTENTS: OnceLock<DashMap<Uuid, PaymentIntent>> = OnceLock::new();


pub(crate) fn intents_store() -> &'static DashMap<Uuid, PaymentIntent> {
    INTENTS.get_or_init(DashMap::new)
}


pub(crate) static TRANSACTIONS: OnceLock<DashMap<Uuid, PaymentTransaction>> = OnceLock::new();


pub(crate) fn transactions_store() -> &'static DashMap<Uuid, PaymentTransaction> {
    TRANSACTIONS.get_or_init(DashMap::new)
}


pub(crate) static METHODS: OnceLock<DashMap<Uuid, PaymentMethod>> = OnceLock::new();


pub(crate) fn methods_store() -> &'static DashMap<Uuid, PaymentMethod> {
    METHODS.get_or_init(DashMap::new)
}


pub(crate) static SUBSCRIPTIONS: OnceLock<DashMap<Uuid, GatewaySubscription>> = OnceLock::new();


pub(crate) fn subscriptions_store() -> &'static DashMap<Uuid, GatewaySubscription> {
    SUBSCRIPTIONS.get_or_init(DashMap::new)
}


pub(crate) static SPLIT_BILLS: OnceLock<DashMap<Uuid, SplitBill>> = OnceLock::new();


pub(crate) fn splits_store() -> &'static DashMap<Uuid, SplitBill> {
    SPLIT_BILLS.get_or_init(DashMap::new)
}


/// Idempotency index: (connection id, idempotency key) -> transaction
/// id. A repeated mutating call with the same key returns the original
/// transaction instead of charging twice.
pub(crate) static IDEMPOTENCY: OnceLock<DashMap<(Uuid, String), Uuid>> = OnceLock::new();


pub(crate) fn idempotency_store() -> &'static DashMap<(Uuid, String), Uuid> {
    IDEMPOTENCY.get_or_init(DashMap::new)
}


/// Processed webhook event ids, with the time they were first seen, so
/// a redelivered webhook is acknowledged but not processed twice.
pub(crate) static WEBHOOK_EVENTS: OnceLock<DashMap<String, DateTime<Utc>>> = OnceLock::new();


/// Drop remembered webhook event ids older than the TTL, so the
/// duplicate-suppression store stays bounded.
pub(crate) fn prune_webhook_events() {
    let cutoff = Utc::now() - chrono::Duration::seconds(WEBHOOK_EVENT_TTL_SECS);
    let stale: Vec<String> = webhook_events_store()
        .iter()
        .filter(|entry| *entry.value() < cutoff)
        .map(|entry| entry.key().clone())
        .collect();
    for id in stale {
        webhook_events_store().remove(&id);
    }
}


pub(crate) fn webhook_events_store() -> &'static DashMap<String, DateTime<Utc>> {
    WEBHOOK_EVENTS.get_or_init(DashMap::new)
}


/// Checkouts started through the store's `payment.initiated` hook,
/// keyed by order id, so the gateway can answer a later confirm
/// without the caller resending the order.
pub(crate) static PENDING_CHECKOUTS: OnceLock<DashMap<String, Value>> = OnceLock::new();


pub(crate) fn pending_checkouts_store() -> &'static DashMap<String, Value> {
    PENDING_CHECKOUTS.get_or_init(DashMap::new)
}


/// Cached PayPal OAuth2 access tokens per connection, with expiry.
pub(crate) static PAYPAL_TOKENS: OnceLock<DashMap<Uuid, (String, DateTime<Utc>)>> = OnceLock::new();


pub(crate) fn paypal_tokens_store() -> &'static DashMap<Uuid, (String, DateTime<Utc>)> {
    PAYPAL_TOKENS.get_or_init(DashMap::new)
}



// ---------------------------------------------------------------------------
// Provider registry: the plug-in point for adapters.
// ---------------------------------------------------------------------------
/// Factory that builds an adapter for one stored connection.
pub type PaymentProviderFactory = Arc<
    dyn Fn(&PaymentConnection) -> UnifiedResult<Arc<dyn PaymentProvider>> + Send + Sync,
>;


pub struct PaymentRegistry {
    pub(crate) factories: RwLock<HashMap<PaymentProviderKind, PaymentProviderFactory>>,
}


impl PaymentRegistry {
    pub(crate) fn new() -> Self {
        Self {
            factories: RwLock::new(HashMap::new()),
        }
    }

    /// Register (or replace) the factory for a kind. This is how a new
    /// provider plugs in: call it once at startup, or from an
    /// extension, instead of editing this module.
    pub fn register_factory(&self, kind: PaymentProviderKind, factory: PaymentProviderFactory) {
        self.factories
            .write()
            .expect("payment registry lock poisoned")
            .insert(kind, factory);
    }

    pub fn build(&self, connection: &PaymentConnection) -> UnifiedResult<Arc<dyn PaymentProvider>> {
        let factories = self
            .factories
            .read()
            .expect("payment registry lock poisoned");
        let factory = factories.get(&connection.kind).ok_or_else(|| {
            UnifiedError::internal(format!(
                "No payment adapter registered for kind \"{}\"",
                connection.kind.as_str()
            ))
        })?;
        factory(connection)
    }

    pub fn registered_kinds(&self) -> Vec<PaymentProviderKind> {
        let factories = self
            .factories
            .read()
            .expect("payment registry lock poisoned");
        let mut kinds: Vec<PaymentProviderKind> = factories.keys().copied().collect();
        kinds.sort_by_key(|kind| kind.as_str());
        kinds
    }
}


pub(crate) static REGISTRY: OnceLock<PaymentRegistry> = OnceLock::new();


/// The shared registry, with the built-in adapters registered on
/// first use.
pub fn payment_registry() -> &'static PaymentRegistry {
    REGISTRY.get_or_init(|| {
        let registry = PaymentRegistry::new();
        for kind in PaymentProviderKind::all() {
            registry.register_factory(
                *kind,
                Arc::new(|connection| {
                    Ok(Arc::new(ProviderAdapter::new(connection)?) as Arc<dyn PaymentProvider>)
                }),
            );
        }
        registry
    })
}
