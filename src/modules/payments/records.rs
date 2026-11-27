//! payments - records.

use super::{
    adapter::*, card_settings::*, card_validation::*, credentials::*, custom_provider::*,
    dispatch::*, events::*, generic_card::*, handlers_catalog::*, handlers_charges::*,
    handlers_checkout::*, handlers_intents::*, handlers_methods::*, handlers_split_bills::*,
    handlers_subscriptions::*, handlers_webhooks::*, hooks::*, hyperswitch::*, limits::*, midtrans::*,
    module::*, mpgs::*, paypal::*, providers::*, reference_ids::*, requests::*, state::*,
    stripe::*, twocheckout::*, types::*, wallets::*, xendit::*,
};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;
use unified_core::db::{
    DbPool,
    models::{
        GatewaySubscription as DbGatewaySubscription, PaymentConnection as DbPaymentConnection,
        PaymentIntent as DbPaymentIntent, PaymentMethod as DbPaymentMethod,
        PaymentTransaction as DbPaymentTransaction, SplitBill as DbSplitBill,
        SplitPart as DbSplitPart,
    },
};
use unified_core::error::{UnifiedError, UnifiedResult};
use uuid::Uuid;



#[derive(Debug, Clone, Serialize)]
// Records: intents, transactions, payment methods, subscriptions.
/// A payment intent: the record a checkout creates first, then
/// confirms. The intent holds no card data - only a token reference.
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
    /// same reason - and so confirm can run the card validation
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
/// expiry - never a PAN or CVV.
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
// Split bills: one order/invoice total divided across methods,
// payers, or instalments.
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



// In-memory stores (same pattern as the provider registry and store).
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

// Database hydration (C11).

/// Hydrate the in-memory payment stores from the database:
/// connections, intents, transactions, methods, gateway
/// subscriptions, and split bills (with their parts) are loaded
/// from the `payment_*` / `gateway_subscriptions` / `split_*`
/// tables and replace the map contents, so a restart restores
/// the money state instead of losing it. The idempotency index
/// is rebuilt from the loaded transactions, so a repeated
/// mutating call after a restart still returns the original
/// transaction instead of charging twice. Rows that cannot be
/// converted - an unknown provider kind / gateway mode, or
/// credentials that do not parse - are skipped, never fatal:
/// hydration must not fail startup on one bad row. The runtime
/// records carry no `tenant_id` field yet, so the `tenant_id`
/// columns added by migration 000126 are ignored here; stamping
/// them is follow-up work (see
/// `docs/integration/INTEGRATION_NOTES_C11.md`). Deliberately
/// NOT hydrated: the webhook-event dedup map (time-bounded,
/// rebuilt from live traffic), pending checkouts (in-flight
/// only), and cached PayPal tokens (expiring credentials cache).
/// Call once from `PaymentsModule::init` after the pool is
/// attached. Returns the number of rows loaded.
pub(crate) fn hydrate_from_db(pool: &DbPool) -> UnifiedResult<usize> {
    use unified_core::db::schema::{
        gateway_subscriptions::dsl as sub_dsl, payment_connections::dsl as conn_dsl,
        payment_intents::dsl as int_dsl, payment_methods::dsl as meth_dsl,
        payment_transactions::dsl as txn_dsl, split_bills::dsl as bill_dsl,
        split_parts::dsl as part_dsl,
    };
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    let db_connections = conn_dsl::payment_connections
        .select(DbPaymentConnection::as_select())
        .load::<DbPaymentConnection>(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot load payment connections: {error}"))
        })?;
    let db_intents = int_dsl::payment_intents
        .select(DbPaymentIntent::as_select())
        .load::<DbPaymentIntent>(&mut conn)
        .map_err(|error| UnifiedError::internal(format!("Cannot load payment intents: {error}")))?;
    let db_transactions = txn_dsl::payment_transactions
        .select(DbPaymentTransaction::as_select())
        .load::<DbPaymentTransaction>(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot load payment transactions: {error}"))
        })?;
    let db_methods = meth_dsl::payment_methods
        .select(DbPaymentMethod::as_select())
        .load::<DbPaymentMethod>(&mut conn)
        .map_err(|error| UnifiedError::internal(format!("Cannot load payment methods: {error}")))?;
    let db_subscriptions = sub_dsl::gateway_subscriptions
        .select(DbGatewaySubscription::as_select())
        .load::<DbGatewaySubscription>(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot load gateway subscriptions: {error}"))
        })?;
    let db_bills = bill_dsl::split_bills
        .select(DbSplitBill::as_select())
        .load::<DbSplitBill>(&mut conn)
        .map_err(|error| UnifiedError::internal(format!("Cannot load split bills: {error}")))?;
    let db_parts = part_dsl::split_parts
        .select(DbSplitPart::as_select())
        .load::<DbSplitPart>(&mut conn)
        .map_err(|error| UnifiedError::internal(format!("Cannot load split parts: {error}")))?;

    let mut loaded = 0;
    connections_store().clear();
    for row in &db_connections {
        let Some(kind) = PaymentProviderKind::from_str(&row.kind) else {
            continue;
        };
        let Some(mode) = GatewayMode::from_str(&row.mode) else {
            continue;
        };
        let Ok(credentials) = serde_json::from_value::<PaymentCredentials>(row.credentials.clone())
        else {
            continue;
        };
        let custom_config = match &row.custom_config {
            Some(value) => match serde_json::from_value::<CustomGatewayConfig>(value.clone()) {
                Ok(config) => Some(config),
                Err(_) => continue,
            },
            None => None,
        };
        connections_store().insert(
            row.id,
            PaymentConnection {
                id: row.id,
                name: row.name.clone(),
                kind,
                mode,
                endpoint: row.endpoint.clone(),
                credentials,
                custom_config,
                delegate_connection_id: row.delegate_connection_id,
                enabled: row.enabled,
                last_tested_at: row.last_tested_at,
                last_status: row.last_status.clone(),
                created_at: row.created_at,
                updated_at: row.updated_at,
            },
        );
        loaded += 1;
    }
    intents_store().clear();
    for row in &db_intents {
        intents_store().insert(
            row.id,
            PaymentIntent {
                id: row.id,
                connection_id: row.connection_id,
                provider: row.provider.clone(),
                customer_id: row.customer_id.clone(),
                order_id: row.order_id.clone(),
                invoice_id: row.invoice_id.clone(),
                amount_minor: row.amount_minor,
                currency: row.currency.clone(),
                status: row.status.clone(),
                capture_method: row.capture_method.clone(),
                provider_transaction_id: row.provider_transaction_id.clone(),
                fee_minor: row.fee_minor,
                payment_method_token: None,
                payment_method_id: None,
                idempotency_key: row.idempotency_key.clone(),
                created_at: row.created_at,
                updated_at: row.updated_at,
            },
        );
        loaded += 1;
    }
    transactions_store().clear();
    idempotency_store().clear();
    for row in &db_transactions {
        transactions_store().insert(
            row.id,
            PaymentTransaction {
                id: row.id,
                intent_id: row.intent_id,
                connection_id: row.connection_id,
                provider: row.provider.clone(),
                kind: row.kind.clone(),
                customer_id: row.customer_id.clone(),
                order_id: row.order_id.clone(),
                invoice_id: row.invoice_id.clone(),
                subscription_id: row.subscription_id,
                amount_minor: row.amount_minor,
                currency: row.currency.clone(),
                fee_minor: row.fee_minor,
                status: row.status.clone(),
                provider_transaction_id: row.provider_transaction_id.clone(),
                provider_refund_id: row.provider_refund_id.clone(),
                original_transaction_id: None,
                operator_id: None,
                idempotency_key: row.idempotency_key.clone(),
                created_at: row.created_at,
            },
        );
        idempotency_store().insert((row.connection_id, row.idempotency_key.clone()), row.id);
        loaded += 1;
    }
    methods_store().clear();
    for row in &db_methods {
        methods_store().insert(
            row.id,
            PaymentMethod {
                id: row.id,
                customer_id: row.customer_id.clone(),
                connection_id: row.connection_id,
                provider: row.provider.clone(),
                provider_token: row.provider_token.clone(),
                brand: row.brand.clone(),
                last4: row.last4.clone(),
                exp_month: row.exp_month.max(0) as u32,
                exp_year: row.exp_year.max(0) as u32,
                is_default: row.is_default,
                created_at: row.created_at,
            },
        );
        loaded += 1;
    }
    subscriptions_store().clear();
    for row in &db_subscriptions {
        subscriptions_store().insert(
            row.id,
            GatewaySubscription {
                id: row.id,
                connection_id: row.connection_id,
                provider: row.provider.clone(),
                customer_id: row.customer_id.clone(),
                order_id: row.order_id.clone(),
                plan_id: row.plan_id.clone(),
                amount_minor: row.amount_minor,
                currency: row.currency.clone(),
                interval: row.interval.clone(),
                interval_count: row.interval_count.max(0) as u32,
                status: row.status.clone(),
                provider_subscription_id: row.provider_subscription_id.clone(),
                payment_method_id: row.payment_method_id,
                current_period_end: row.current_period_end,
                next_renewal_at: row.next_renewal_at,
                cancelled_at: row.cancelled_at,
                // Dunning state is not stored in the database
                // yet: a hydrated subscription restarts dunning
                // from zero. The renewal path, not the stored
                // counters, decides the next charge.
                dunning_max_retries: 0,
                dunning_retry_interval_days: 0,
                grace_period_days: 0,
                consecutive_failures: 0,
                dunning_exhausted: false,
                grace_ends_at: None,
                created_at: row.created_at,
                updated_at: row.updated_at,
            },
        );
        loaded += 1;
    }
    let mut parts_by_bill: std::collections::HashMap<Uuid, Vec<SplitPart>> =
        std::collections::HashMap::new();
    for part in &db_parts {
        parts_by_bill
            .entry(part.split_id)
            .or_default()
            .push(SplitPart {
                id: part.id,
                split_id: part.split_id,
                index: part.index.max(0) as usize,
                amount_minor: part.amount_minor,
                currency: part.currency.clone(),
                provider: part.provider.clone(),
                connection_id: part.connection_id,
                payer_id: part.payer_id.clone(),
                due_date: part.due_date,
                status: part.status.clone(),
                paid_transaction_id: part.paid_transaction_id,
                paid_at: part.paid_at,
            });
    }
    for parts in parts_by_bill.values_mut() {
        parts.sort_by_key(|part| part.index);
    }
    splits_store().clear();
    for row in &db_bills {
        let parts = parts_by_bill.remove(&row.id).unwrap_or_default();
        splits_store().insert(
            row.id,
            SplitBill {
                id: row.id,
                order_id: row.order_id.clone(),
                invoice_id: row.invoice_id.clone(),
                customer_id: row.customer_id.clone(),
                total_minor: row.total_minor,
                currency: row.currency.clone(),
                status: row.status.clone(),
                parts,
                paid_minor: row.paid_minor,
                remaining_minor: row.remaining_minor,
                created_at: row.created_at,
                updated_at: row.updated_at,
            },
        );
        loaded += 1;
    }
    Ok(loaded)
}

// Provider registry: the plug-in point for adapters.
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
