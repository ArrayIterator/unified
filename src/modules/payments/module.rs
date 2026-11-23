//! payments - module.

// Payment Gateway module — a wrapper / framework, not a single provider.
//
// One hookable provider registry with a common interface (`authorize`,
// `capture`, `charge`, `refund`, `void`, `subscription_create`,
// `subscription_cancel`, `webhook_verify`, `payout`), so new gateways
// plug in through the registry instead of scattered code. Every adapter
// is an honest, typed HTTP client: it validates its configuration,
// tests the connection against the real provider API, and maps provider
// responses onto the shared result shapes. No adapter pretends to be a
// full vendor SDK, and no adapter invents a provider capability it does
// not have — the per-provider capability matrix is the single source of
// truth and is exposed by the catalog endpoint.
//
// Scope per provider (honest, documented on each adapter function):
//
//   - Xendit: card charges (one-time, tokenized), capture, refunds,
//     disbursements (payouts). Subscriptions are a local schedule over
//     tokenized charges: Xendit exposes no single subscription object
//     to this adapter, so the subscription record lives here and each
//     period is a charge against the stored card token.
//   - HyperPay (OPPWA): checkout creation plus a status fetch (the
//     shopper completes payment in the HyperPay hosted widget using the
//     returned checkout id), authorize (PA) / capture (CP) / refund /
//     void through the payments endpoint. Subscriptions are a local
//     schedule over tokenized charges.
//   - 2Checkout (2CO): orders placed with a client-side token
//     (EesToken), refunds, subscription as a local schedule over
//     tokenized charges. Webhook verification is a server-side sale
//     lookup: the native INS signature is MD5-based and no MD5
//     implementation is available in this codebase, so a webhook is
//     only trusted after the sale is re-fetched from the 2CO API.
//   - Generic card processing: a configurable acquiring endpoint
//     supplied on the connection. The processor must accept the
//     documented JSON contract (see `CardAdapter`); raw card numbers
//     are never accepted — only tokens from a PCI-compliant
//     tokenization step upstream.
//   - Mastercard gateway (MPGS): PAY / AUTHORIZE / CAPTURE / REFUND /
//     VOID operations against a merchant gateway host, 3-D Secure
//     through the provider redirect. Subscriptions are a local
//     schedule over tokenized charges.
//   - Google Pay / Apple Pay: wallet adapters. They never talk to a
//     wallet API directly (there is no server-side wallet API to
//     charge); they forward the wallet payment token to a configured
//     delegate gateway connection, which performs the charge. The
//     delegate's currency list and capabilities apply.
//   - PayPal: Orders API v2 (create + capture, authorize + capture),
//     refunds, Billing Subscriptions, payouts. Webhook verification
//     calls PayPal's verify-webhook-signature endpoint.
//   - Stripe: PaymentIntents (authorize with manual capture, charge,
//     cancel as void), refunds, native subscriptions, transfers
//     (payouts). Webhook verification is HMAC-SHA256 over
//     "{timestamp}.{payload}" (Stripe-Signature header).
//   - Midtrans: Core API charges (card token, bank transfer,
//     e-wallet), capture, refunds, native subscriptions
//     (`/v1/subscriptions`). Webhook verification is the documented
//     SHA512 signature over order id + status code + gross amount +
//     server key.
//   - Custom: a config-driven generic adapter — base URL, auth type,
//     endpoint paths, and response field mapping supplied on the
//     connection, so a new gateway plugs in without a code change.
//     Hosts are allow-listed by the operator-supplied https endpoint;
//     http endpoints are refused.
//
// Credentials live only on the connection record, are never logged,
// and are never returned by any API response: read views expose only
// which credential fields are configured. Payment methods are
// tokenized only: the stored record holds the provider token, the
// card brand, last four digits and expiry — never a PAN or CVV. A
// token that looks like a card number (13-19 digits) is rejected.
//
// Money is always integer minor units (cents) plus an ISO 4217
// currency code. Fees are recorded only when the provider reports
// them; they are never estimated.
//
// A 429 from a provider is surfaced as a 429 with the provider's
// `Retry-After` delay in `error.metadata`. Provider errors are mapped
// into the standard error envelope with the provider code in
// metadata; provider response bodies are never leaked into the
// client-visible message.
//
// Events emitted on the hook bus: `payment.authorized`,
// `payment.captured`, `payment.refunded`, `payment.failed`,
// `payment.subscription.created`, `payment.subscription.cancelled`,
// `payment.subscription.renewed`, `payment.webhook.received`,
// `payment.chargeback`, `payment.split.created`,
// `payment.split.part.paid`, `payment.split.completed`, and
// `payment.split.cancelled`. Bank transfer is a first-class provider
// kind here (`bank_transfer`): its adapter never moves money over an
// API — a charge only opens a pending manual verification (handled
// by the bank-transfer module), an administrator approval is the
// capture and emits `payment.captured`, and refund / void are manual
// operations recorded with an audit note. This module also listens
// to the store's
// `payment.initiated` (a checkout started through
// `POST /api/v1/store/orders/{id}/pay`) and to
// `store.order.refunded`, and to
// `payment.subscription.renew_requested` (renewal trigger; the actual
// renewal charge runs through the HTTP renew endpoint because hook
// handlers are synchronous).
//
// Wiring to store / billing / licensing happens through hooks only —
// this module never calls another module directly. The exact listener
// snippets the integrator needs are in the coordinator report; the
// short version: the store's `payment.initiated` is answered here, a
// capture emits `payment.captured` (billing records gateway,
// transaction id, fees and status from it), a refund emits
// `payment.refunded` (licensing already revokes on
// `store.order.refunded`; it needs the same listener on
// `payment.refunded` for gateway-first refunds), and failed /
// chargeback webhooks emit `payment.failed` / `payment.chargeback`
// plus a `notification.requested` event.
//
// Card validation (batch 18) is a gate, not a provider feature:
// before a card payment is processed, the gateway emits
// `cards.validate_requested` on the hook bus and proceeds ONLY on
// a successful validation answer from the cards module; a missing
// or negative answer rejects the payment (see
// `request_card_validation`). Raw card details are accepted ONLY
// through the optional `card` object on the checkout requests, are
// held in memory for that one validation call, and are never
// stored, logged, or returned. The token boundary is unchanged: a
// value that looks like a card number is still rejected as a
// `payment_method_token` by `validate_token`. Payment reference
// settings (default prefix, per-product prefixes, total-length
// cap) live in `unified_core::reference_id::ReferenceIdSettings`
// and are owned by the store and billing modules, edited through
// their reference-settings endpoints (changes emitted as
// `reference.settings_changed`). The gateway keeps no
// `reference_id.*` settings surface of its own: it generates
// references with the shared defaults only (see
// `charge_reference`), and the store / billing endpoints are the
// only live read paths for generation. Likewise, card validation
// settings are owned per tenant by the cards module
// (`PUT /api/v1/cards/settings`: test_mode live/sandbox,
// max_expiry_years_ahead 1-50, gpn_enabled,
// disabled_standard_brands) — the gateway keeps no `cards.*`
// settings surface either. The gateway's old `cards.*` /
// `reference_id.*` settings namespace was DROPPED (batch-18
// integration decision), not mirrored: it used a divergent
// allowlist (`enabled_brands`) and bound (max 30), and a global
// (tenant-less) mirror cannot track the cards module's per-tenant
// values — the validator reads its own per-tenant settings and
// ignores anything the gateway sends — so any mirror would go
// stale on the first cards-settings edit and present stale values
// as effective. Writes to either namespace through the settings
// module are refused with 422 by `on_config_validate_cards`,
// which points the caller at the canonical endpoint, instead of
// silently doing nothing. The provider-facing reference for every
// charge is resolved by `charge_reference` in this module
// (threaded store reference, else the order id when it already is
// a valid reference, else a freshly generated one).
//
// Persistence: records are kept in memory (same pattern as the
// provider registry and the store). Durable persistence is a
// follow-up for the integrator — see the migration sketch in the
// coordinator report.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::{web};
use std::sync::{Arc};
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperpay::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*};
use super::ai_capabilities::AI_CAPABILITIES;



// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------
pub struct PaymentsModule {
    pub(crate) state: Arc<PaymentsState>,
}


impl PaymentsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(PaymentsState::new()),
        }
    }
}


impl Default for PaymentsModule {
    fn default() -> Self {
        Self::new()
    }
}


impl Module for PaymentsModule {
    fn id(&self) -> &'static str {
        "payments"
    }

    fn name(&self) -> &'static str {
        "Payment Gateway"
    }

    fn description(&self) -> &'static str {
        "Payment gateway wrapper: Xendit, HyperPay, 2Checkout, card processing, Mastercard, Google Pay, Apple Pay, PayPal, Stripe, Midtrans, custom providers, and bank transfer (manual verification) behind one hookable interface, plus split bills across methods, payers, and instalments."
    }

    fn requires(&self) -> &[&'static str] {
        &["store", "billing", "licensing", "scheduler"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["reqwest", "hmac", "sha2", "uuid", "chrono", "async-trait"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("payments", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "payment.initiated",
            "payments",
            Arc::new(move |ctx: &mut HookContext| on_payment_initiated(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "store.order.refunded",
            "payments",
            Arc::new(move |ctx: &mut HookContext| on_store_order_refunded(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            PAYMENT_CAPTURED,
            "payments",
            Arc::new(move |ctx: &mut HookContext| {
                on_payment_captured_for_split(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "payment.subscription.renew_requested",
            "payments",
            Arc::new(move |ctx: &mut HookContext| {
                on_subscription_renew_requested(state.clone(), ctx)
            }),
        );
        // Batch 18: refuse `cards.*` / `reference_id.*` admin
        // writes through the settings module. The gateway keeps no
        // settings surface for either namespace (single source of
        // truth: the cards module per tenant, and the store /
        // billing modules for reference ids), so `config.validate`
        // rejects those writes with 422 and points at the
        // canonical endpoint. There is deliberately no
        // `settings.changed` listener here: nothing is mirrored.
        bus.register(
            events::CONFIG_VALIDATE,
            "payments",
            Arc::new(on_config_validate_cards),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/payments")
                .route("/providers", web::get().to(list_providers))
                .route("/providers/{kind}", web::get().to(get_provider))
                .route("/providers/{kind}/settings", web::get().to(get_provider_settings))
                .route("/cards/settings", web::get().to(get_cards_settings))
                .route("/connections", web::get().to(list_connections))
                .route("/connections", web::post().to(create_connection))
                .route("/connections/{id}", web::get().to(get_connection))
                .route("/connections/{id}", web::put().to(update_connection))
                .route("/connections/{id}", web::delete().to(delete_connection))
                .route("/connections/{id}/test", web::post().to(test_connection))
                .route("/connections/{id}/mode", web::post().to(switch_mode))
                .route("/intents", web::post().to(create_intent))
                .route("/intents/{id}", web::get().to(get_intent))
                .route("/intents/{id}/confirm", web::post().to(confirm_intent))
                .route("/intents/{id}/capture", web::post().to(capture_intent))
                .route("/intents/{id}/cancel", web::post().to(cancel_intent))
                .route("/charges", web::post().to(create_charge))
                .route("/transactions", web::get().to(list_transactions))
                .route("/transactions/{id}", web::get().to(get_transaction))
                .route("/transactions/{id}/refund", web::post().to(refund_transaction))
                .route("/transactions/{id}/complete-refund", web::post().to(complete_refund))
                .route("/transactions/{id}/void", web::post().to(void_transaction))
                .route("/payouts", web::post().to(create_payout))
                .route("/methods", web::get().to(list_payment_methods))
                .route("/methods", web::post().to(create_payment_method))
                .route("/methods/{id}", web::get().to(get_payment_method))
                .route("/methods/{id}", web::delete().to(delete_payment_method))
                .route("/methods/{id}/set-default", web::post().to(set_default_payment_method))
                .route("/subscriptions", web::get().to(list_subscriptions))
                .route("/subscriptions", web::post().to(create_subscription))
                .route("/subscriptions/renew-due", web::post().to(renew_due_subscriptions))
                .route("/subscriptions/{id}", web::get().to(get_subscription))
                .route("/subscriptions/{id}/cancel", web::post().to(cancel_subscription))
                .route("/subscriptions/{id}/renew", web::post().to(renew_subscription))
                .route("/orders/{order_id}/pay", web::post().to(pay_order))
                .route("/splits", web::get().to(list_splits))
                .route("/splits", web::post().to(create_split))
                .route("/splits/schedule", web::post().to(generate_schedule))
                .route("/splits/{id}", web::get().to(get_split))
                .route("/splits/{id}/parts/{part_id}/pay", web::post().to(pay_split_part))
                .route("/splits/{id}/cancel", web::post().to(cancel_split))
                .route("/webhooks/events", web::get().to(list_webhook_events))
                .route("/webhooks/{provider}", web::post().to(webhook_receiver)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/payments/providers", owner: "payments", description: "Provider catalog with capability matrix, currencies, and settings fields", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/payments/providers/{kind}", owner: "payments", description: "One provider's catalog entry", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/payments/providers/{kind}/settings", owner: "payments", description: "Settings-page field definitions for one provider", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/payments/cards/settings", owner: "payments", description: "Card validation contract: capabilities, validation event and reason codes, and pointers to the canonical card and reference-id settings surfaces (admin-only, cards:read)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/payments/connections", owner: "payments", description: "List provider connections (credentials masked)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/connections", owner: "payments", description: "Create a provider connection", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/payments/connections/{id}", owner: "payments", description: "Get one connection (credentials masked)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/payments/connections/{id}", owner: "payments", description: "Update a connection (credentials merged, never returned)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/payments/connections/{id}", owner: "payments", description: "Delete a connection", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/connections/{id}/test", owner: "payments", description: "Test a connection against the real provider API", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/connections/{id}/mode", owner: "payments", description: "Switch a connection between sandbox and live mode", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/intents", owner: "payments", description: "Create a payment intent", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/payments/intents/{id}", owner: "payments", description: "Get one payment intent", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/intents/{id}/confirm", owner: "payments", description: "Confirm an intent (authorize or charge)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/intents/{id}/capture", owner: "payments", description: "Capture an authorized intent", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/intents/{id}/cancel", owner: "payments", description: "Cancel an intent (voids an authorization at the provider)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/charges", owner: "payments", description: "Charge a tokenized payment method directly", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/payments/transactions", owner: "payments", description: "List transactions with provider/status/currency/customer/date filters", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/payments/transactions/{id}", owner: "payments", description: "Get one transaction", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/transactions/{id}/refund", owner: "payments", description: "Refund a captured charge (gateway first, then the refund event)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/transactions/{id}/complete-refund", owner: "payments", description: "Complete a pending refund (refunded or failed) and emit the refund event", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/transactions/{id}/void", owner: "payments", description: "Void an authorization", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/payouts", owner: "payments", description: "Pay out through a provider that supports payouts", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/payments/methods", owner: "payments", description: "List tokenized payment methods per customer", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/methods", owner: "payments", description: "Store a tokenized payment method (never a PAN)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/payments/methods/{id}", owner: "payments", description: "Get one payment method", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/payments/methods/{id}", owner: "payments", description: "Delete a payment method", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/methods/{id}/set-default", owner: "payments", description: "Mark a payment method as the customer default", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/payments/subscriptions", owner: "payments", description: "List gateway subscriptions", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/subscriptions", owner: "payments", description: "Create a gateway subscription", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/subscriptions/renew-due", owner: "payments", description: "Renew every subscription whose renewal time has arrived", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/payments/subscriptions/{id}", owner: "payments", description: "Get one subscription", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/subscriptions/{id}/cancel", owner: "payments", description: "Cancel a subscription", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/subscriptions/{id}/renew", owner: "payments", description: "Renew one subscription now", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/orders/{order_id}/pay", owner: "payments", description: "Pay for a store order through the gateway", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/payments/splits", owner: "payments", description: "List split bills with pagination and filters", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/splits", owner: "payments", description: "Create a split bill; parts must sum exactly to the total (422 with per-part math otherwise)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/splits/schedule", owner: "payments", description: "Generate an instalment schedule (equal parts or custom amounts, due dates), optionally creating the split", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/payments/splits/{id}", owner: "payments", description: "Get one split bill with its parts", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/splits/{id}/parts/{part_id}/pay", owner: "payments", description: "Pay one split part through any provider, including bank transfer (manual verification)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/splits/{id}/cancel", owner: "payments", description: "Cancel a split bill (only while no part is paid)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/payments/webhooks/{provider}", owner: "payments", description: "Receive a provider webhook (signature verified first)", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/payments/webhooks/events", owner: "payments", description: "List processed webhook event ids", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}


pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(PaymentsModule::new()));
}
