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
// cap) live in `crate::core::reference_id::ReferenceIdSettings`
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

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::reference_id;
use crate::core::tenancy::TenantContext;
use actix_web::{web, HttpRequest, HttpResponse};
use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Datelike, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256, Sha512};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

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

// ---------------------------------------------------------------------------
// Batch 18 — card validation wiring.
//
// The gateway never validates a card number itself: it emits
// `cards.validate_requested` and the cards module answers. This
// section is the whole wiring surface — the event contract, the
// reason codes, the capability names, and the fail-closed gate
// (`request_card_validation`) the checkout handlers call before
// any card payment is processed. The cards module itself
// (validation, brand data, custom rules, and its per-tenant
// settings) and the reference-id generator live in their own
// modules; this section only wires them into the payment flow.
// The gateway keeps no `cards.*` / `reference_id.*` settings of
// its own: those namespaces were dropped (see the module header
// and `on_config_validate_cards`) because the cards module's
// per-tenant settings and the store / billing reference settings
// are the single sources of truth.
// ---------------------------------------------------------------------------

/// Emitted before a card payment is processed, asking the cards
/// module to validate the card. The gateway proceeds ONLY on a
/// successful validation answer; see `request_card_validation`.
/// WARNING: the payload carries the card number (`number`,
/// digits only), the expiry (`expiry`, `"MM/YYYY"`), and the CVV
/// (`cvv`, the value) so the validator can run Luhn, brand
/// detection, expiry, and CVV-length checks. All three travel on
/// the in-process hook bus only: the cards module's validator is
/// pure — it never stores, logs, or returns them — and the
/// payload must never be audited, persisted, or forwarded to a
/// webhook. `cards.validate_requested` is therefore deliberately
/// absent from the audit event list and from the webhooks
/// known-events list; only the `cards.validated` outcome event
/// (masked PAN only) is auditable.
pub const CARDS_VALIDATE_REQUESTED: &str = "cards.validate_requested";
/// Emitted by the cards module (not by this gateway) once per
/// answered validation, with the masked PAN only — never the full
/// number. The audit log subscribes to this event, not to the
/// request event above. Payload (cards module): `tenant_id`,
/// `masked_pan`, `brand_id`, `brand_name`, `matched_rule`,
/// `valid`, `reason_codes`, `is_test_number`.
pub const CARDS_VALIDATED: &str = "cards.validated";
/// Emitted by the cards module when an admin creates, updates, or
/// deletes a custom card validation rule. NOTE: the cards module
/// does not emit this event yet (no emit site in `cards.rs` at
/// batch-18 time); the audit snippet lists it as a required
/// addition for the cards module, not as a live event.
pub const CARDS_CUSTOM_RULE_CHANGED: &str = "cards.custom_rule.changed";
/// Emitted by the reference-id generator when it mints a
/// reference order id. The event name is the generator's own
/// (`reference.generated`, see `reference_id::
/// EVENT_REFERENCE_GENERATED`); the payload is `{ "reference",
/// "prefix", "tenant_id" }`.
pub const REFERENCE_ID_GENERATED: &str = "reference.generated";
/// Emitted after the reference-id settings change (see
/// `reference_id::EVENT_REFERENCE_SETTINGS_CHANGED`).
pub const REFERENCE_SETTINGS_CHANGED: &str = "reference.settings_changed";

/// Card validation failed the Luhn checksum.
pub const CARD_REASON_INVALID_LUHN: &str = "invalid_luhn";
/// The card number matches no known brand (and no custom rule).
pub const CARD_REASON_UNKNOWN_BRAND: &str = "unknown_brand";
/// The card number has the wrong length for its brand.
pub const CARD_REASON_INVALID_LENGTH: &str = "invalid_length";
/// The expiry date is past, malformed, or beyond the upper bound
/// in the cards module's per-tenant settings
/// (`max_expiry_years_ahead`).
pub const CARD_REASON_INVALID_EXPIRY: &str = "invalid_expiry";
/// The CVV has the wrong length or format for the brand.
pub const CARD_REASON_INVALID_CVV: &str = "invalid_cvv";
/// The detected brand is disabled by the cards module's
/// per-tenant settings.
pub const CARD_REASON_BRAND_DISABLED: &str = "brand_disabled";
/// The number is a known test number and the tenant is in live
/// mode (the cards module's per-tenant `test_mode` is `live`).
pub const CARD_REASON_TEST_NUMBER_REJECTED: &str = "test_number_rejected";
/// The number is malformed (illegal characters, or missing where
/// the validator requires a number). The gateway pre-checks the
/// shape, so this surfaces mainly when a number-less payload
/// reaches a validator that requires one (method / claim modes
/// before the cards handler learns them).
pub const CARD_REASON_INVALID_FORMAT: &str = "invalid_format";
/// The cards module did not answer (not registered, silent, or an
/// answer that breaks the contract). Fail closed, never proceed.
pub const CARD_REASON_UNAVAILABLE: &str = "cards_unavailable";

/// Every reason code a validation answer may carry. An answer
/// whose reason is outside this list is a contract violation
/// and is treated as `cards_unavailable`. The cards module's
/// `missing_tenant_context` refusal is not a validation reason:
/// it maps to `cards_unavailable` (the gate guarantees a tenant
/// id is always present, so it should never fire).
pub const CARD_VALIDATION_REASONS: &[&str] = &[
    CARD_REASON_INVALID_LUHN,
    CARD_REASON_UNKNOWN_BRAND,
    CARD_REASON_INVALID_LENGTH,
    CARD_REASON_INVALID_EXPIRY,
    CARD_REASON_INVALID_CVV,
    CARD_REASON_BRAND_DISABLED,
    CARD_REASON_TEST_NUMBER_REJECTED,
    CARD_REASON_INVALID_FORMAT,
    CARD_REASON_UNAVAILABLE,
];

/// Capability: read card validation settings and the validation contract.
pub const CARDS_READ: &str = "cards:read";
/// Capability: validate a card (server-side checkout gate and the
/// admin validation flow). Never granted to unprivileged roles:
/// an open validation oracle lets anyone probe card numbers.
pub const CARDS_VALIDATE: &str = "cards:validate";
/// Capability: manage card validation settings and custom rules.
pub const CARDS_MANAGE: &str = "cards:manage";

/// Coarse upper bound for the local expiry-year shape guard in
/// `create_payment_method`: the cards module accepts
/// `max_expiry_years_ahead` 1-50 per tenant, so a card expiring
/// more than 50 years past the current year can never validate.
/// This guard only rejects years no tenant setting could ever
/// accept; the validator stays authoritative for the real
/// per-tenant bound and for past-ness (`invalid_expiry`). There
/// is deliberately no settings-driven bound here: the gateway
/// keeps no `cards.*` settings surface (see the module header),
/// so a gateway-side bound would drift from the cards module's
/// per-tenant value.
pub const CARDS_EXPIRY_YEAR_GUARD_MAX_AHEAD: u32 = 50;

/// Raw card details for one validation, supplied on a checkout
/// request. All fields are optional and defaulted so older
/// clients that send no `card` object keep working: the gate
/// only runs when `number` is non-empty. The struct is
/// deserialize-only and is never serialized anywhere — the number,
/// expiry, and CVV value leave this handler only as the
/// in-memory `cards.validate_requested` bus payload (`number`,
/// `expiry`, `cvv`) to the cards module's pure validator, and
/// all three are dropped with the request. Never store, log, or
/// return these fields.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CardDetailsInput {
    /// Card number; spaces and dashes are tolerated and stripped.
    #[serde(default)]
    pub number: String,
    /// Expiry month, 1-12.
    #[serde(default)]
    pub exp_month: u32,
    /// Expiry year, four digits.
    #[serde(default)]
    pub exp_year: u32,
    /// Card verification value. It is sent to the cards module
    /// only as the `cvv` field of the in-memory
    /// `cards.validate_requested` payload (the validator needs
    /// the value for its length check); it is never stored,
    /// logged, or returned.
    #[serde(default)]
    pub cvv: Option<String>,
}

/// Outcome of a successful card validation: the mode that ran and
/// the validator's brand and masked PAN, when the answer carried
/// them. `Ok(Some(_))` from `request_card_validation` always
/// means the card is valid; failures are errors, never a
/// `valid: false` outcome.
#[derive(Debug, Clone)]
pub struct CardValidationOutcome {
    /// `"full"` (raw card details), `"method"` (stored method),
    /// or `"claim"` (claimed brand / last4 / expiry on
    /// method onboarding).
    pub mode: String,
    /// Brand detected by the validator, when the answer carried it.
    pub brand: Option<String>,
    /// Masked PAN (first 6 + last 4 in full mode, `******` + last
    /// 4 in method mode), when known.
    pub masked_pan: Option<String>,
}

/// Keep only the ASCII digits of a value (strips the spaces and
/// dashes callers use to group a card number).
pub fn digits_only(value: &str) -> String {
    value.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// Mask a card number for logs, errors, and audit records: first 6
/// digits, six asterisks, last 4 digits (`411111******1111`).
/// Shorter inputs mask everything except the last 4 digits; an
/// input shorter than 4 digits masks to `****`. The full number is
/// never returned.
pub fn mask_pan(pan_digits: &str) -> String {
    let digits = digits_only(pan_digits);
    if digits.len() < 4 {
        return "*".repeat(4);
    }
    let last4 = &digits[digits.len() - 4..];
    if digits.len() >= 10 {
        format!("{}******{}", &digits[..6], last4)
    } else {
        format!("{}{}", "*".repeat(digits.len() - 4), last4)
    }
}

// ---------------------------------------------------------------------------
// Card settings: the gateway keeps none (single source of truth).
// ---------------------------------------------------------------------------
//
// The gateway's old `cards.*` / `reference_id.*` settings
// namespace (definitions, effective-value store, `config.validate`
// arms, `settings.changed` mirror) was DROPPED, not mirrored —
// see the module header and `on_config_validate_cards`. Card
// validation settings are owned per tenant by the cards module
// (`PUT /api/v1/cards/settings`); reference-id settings are owned
// by the store and billing modules. The only card-settings
// behaviour left in this module is the coarse expiry-year guard
// constant above, used by `create_payment_method`.

// ---------------------------------------------------------------------------
// Card / reference-id settings: the gateway owns none (refuse writes).
// ---------------------------------------------------------------------------

/// `config.validate` handler for the `cards.*` and
/// `reference_id.*` namespaces: every admin write to these
/// namespaces through the settings module is refused with 422.
///
/// Single-source-of-truth decision (batch-18 integration): the
/// cards module owns card validation settings per tenant
/// (`PUT /api/v1/cards/settings`: `test_mode` live/sandbox,
/// `max_expiry_years_ahead` 1-50, `gpn_enabled`,
/// `disabled_standard_brands`), and the store / billing modules
/// own reference-id settings (`PUT /api/v1/store/reference-settings`
/// and `PUT /api/v1/billing/reference-settings`). The gateway
/// keeps no settings surface for either namespace — its old
/// namespace was dropped, not mirrored (see the module header).
/// Accepting a write here would silently do nothing (the
/// validator reads only its own per-tenant settings; the gateway
/// never reads these keys), so the write is refused and the
/// caller is pointed at the canonical endpoint instead of being
/// left believing the value took effect. The handler only
/// validates — it never applies a value — and there is no
/// `settings.changed` listener left in this module.
fn on_config_validate_cards(ctx: &mut HookContext) {
    let namespace = ctx
        .get("namespace")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if namespace != "cards" && namespace != "reference_id" {
        return;
    }
    let key = ctx
        .get("key")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let message = if namespace == "cards" {
        format!(
            "Setting \"cards.{key}\" is owned by the cards module; edit it with PUT /api/v1/cards/settings (test_mode, max_expiry_years_ahead, gpn_enabled, disabled_standard_brands). The gateway keeps no cards.* settings."
        )
    } else {
        format!(
            "Setting \"reference_id.{key}\" is owned by the store and billing modules; edit it with PUT /api/v1/store/reference-settings or PUT /api/v1/billing/reference-settings. The gateway keeps no reference_id.* settings."
        )
    };
    ctx.set("valid", json!(false));
    ctx.set("message", json!(message));
}

// ---------------------------------------------------------------------------
// Card validation: capability check and the checkout gate.
// ---------------------------------------------------------------------------

/// Fail when the caller does not hold `permission` for the cards
/// resource. Follows the entitlements module's `role.check`
/// pattern: ask the hook bus, fail closed on silence (no roles
/// module answering means denied). The role comes from the
/// `x-role` header — the same trusted-header caveat as the rest of
/// the codebase applies until the auth extractor lands. Used by
/// the cards settings endpoint; the checkout gate itself runs
/// server-side on behalf of the paying customer (ownership is
/// enforced by `require_customer` at the call sites) and does not
/// take a role.
fn require_cards_capability(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<()> {
    let denied = || {
        UnifiedError::forbidden(format!(
            "Role lacks required capability: {permission}"
        ))
    };
    let Some(role) = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err(denied());
    };
    let outcome = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    if outcome
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        Ok(())
    } else {
        Err(denied())
    }
}

/// Fail-closed error for a card payment rejected because the
/// validator could not answer: the cards module is not
/// registered, it stayed silent, or its answer broke the
/// contract. 503, with the generic reason code and the masked PAN
/// only — never the full number, never a per-digit detail.
fn cards_unavailable(masked_pan: Option<&str>) -> UnifiedError {
    let mut error = UnifiedError::new(
        "Card validation is unavailable; the card payment was rejected",
        503,
    )
    .with_metadata(
        "reason",
        Value::String(CARD_REASON_UNAVAILABLE.to_string()),
    );
    if let Some(masked) = masked_pan {
        error = error.with_metadata("masked_pan", Value::String(masked.to_string()));
    }
    error
}

/// Card data the validator can check without a PAN: a brand,
/// last 4 digits, and expiry — as claimed on a method-onboarding
/// request. The stored-method path resolves the same fields from
/// the stored record instead; the two shapes are identical on the
/// wire, only the source (and the mode label) differs.
#[derive(Debug, Clone)]
pub struct CardMethodRef {
    pub brand: String,
    pub last4: String,
    pub exp_month: u32,
    pub exp_year: u32,
}

/// Ask the cards module to validate the card for one checkout and
/// return the outcome, or reject the payment.
///
/// The gate runs only when the gateway actually holds card data:
///
///   - raw `card` details in the request → mode `"full"`: Luhn,
///     brand detection, per-brand length, expiry, CVV length,
///     brand enabled, test-number policy;
///   - a stored `payment_method_id` (and no raw details) → mode
///     `"method"`: brand known / enabled and expiry, from the
///     stored brand, last4, and expiry (no PAN exists to run Luhn
///     over);
///   - a claimed brand / last4 / expiry on a method-onboarding
///     request (and no raw details) → mode `"claim"`: the same
///     brand and expiry checks as `"method"`, so a disabled brand
///     or an out-of-bound expiry is rejected at onboarding instead
///     of at the first charge;
///   - an opaque token alone → no gate: the token came from a
///     PCI-compliant tokenization step upstream and the gateway
///     cannot validate what it cannot see. `Ok(None)` means
///     "nothing to validate", not "validation passed".
///
/// KNOWN GAP (batch-18 integrator item): the cards module's hook
/// handler currently requires `number` and answers
/// `invalid_format` when it is missing, so `"method"` and
/// `"claim"` emits fail closed with `invalid_format` until the
/// handler learns number-less validation (brand / last4 / expiry
/// only). The gate keeps emitting them anyway — silently
/// skipping stored-method validation would be a fail-open — so
/// stored-method card payments stay rejected until that handler
/// extension lands. See the batch report for the exact
/// extension.
///
/// Fail closed, in order: no resolved tenant context (401/403
/// from the tenant extractor — card payments are tenant-scoped
/// and never fall back to the control plane); the cards module
/// is not registered on `cards.validate_requested` (503
/// `cards_unavailable`); the module stays silent or answers
/// without a boolean `valid` (503 `cards_unavailable`); the
/// answer is invalid (422 with the generic reason code from the
/// allowlist, the masked PAN only, and the brand when the answer
/// carried one); the answer carries a reason outside the
/// allowlist (503 `cards_unavailable` — a contract violation,
/// never a guessed reason). The full PAN and the CVV value are
/// never stored, logged, or returned: they travel only on the
/// in-process bus to the cards module's pure validator
/// (`number`, `expiry`, `cvv`), which never stores or logs them,
/// and the request event is never audited or forwarded.
#[allow(clippy::too_many_arguments)]
fn request_card_validation(
    state: &PaymentsState,
    req: &HttpRequest,
    card: Option<&CardDetailsInput>,
    method_id: Option<Uuid>,
    claimed: Option<CardMethodRef>,
    connection: &PaymentConnection,
    customer_id: &str,
    order_id: Option<&str>,
    intent_id: Option<Uuid>,
) -> UnifiedResult<Option<CardValidationOutcome>> {
    let tenant = TenantContext::from_http_request(req)?;
    let full = card
        .map(|details| !details.number.trim().is_empty())
        .unwrap_or(false);
    let method = if full {
        None
    } else {
        method_id.and_then(|id| {
            methods_store()
                .get(&id)
                .map(|entry| entry.value().clone())
        })
    };
    if !full && method.is_none() && claimed.is_none() {
        return Ok(None);
    }
    let mode = if full {
        "full"
    } else if method.is_some() {
        "method"
    } else {
        "claim"
    };
    let (pan_digits, masked_pan, brand, exp_month, exp_year, cvv_present, cvv_length, cvv_digits_only, claimed_last4, cvv_value) =
        if full {
            let details = card.expect("full mode implies card details are present");
            if details.number.trim().len() > 64 {
                return Err(UnifiedError::bad_request(
                    "Card number is too long",
                ));
            }
            let digits = digits_only(&details.number);
            if digits.is_empty()
                || !details
                    .number
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == ' ' || c == '-')
            {
                return Err(UnifiedError::bad_request(
                    "Card number must contain only digits, spaces, or dashes",
                ));
            }
            if !(1..=12).contains(&details.exp_month) {
                return Err(UnifiedError::bad_request("exp_month must be 1-12"));
            }
            if !(1000..=9999).contains(&details.exp_year) {
                return Err(UnifiedError::bad_request(
                    "exp_year must be a four-digit year",
                ));
            }
            let cvv = details.cvv.clone().unwrap_or_default();
            if cvv.len() > 8 {
                return Err(UnifiedError::bad_request("CVV is too long"));
            }
            let masked = mask_pan(&digits);
            (
                Some(digits),
                Some(masked),
                None,
                details.exp_month,
                details.exp_year,
                !cvv.is_empty(),
                cvv.len(),
                cvv.chars().all(|c| c.is_ascii_digit()),
                None,
                if cvv.is_empty() { None } else { Some(cvv) },
            )
        } else {
            // Method mode resolves the stored record (ownership is
            // re-checked here even though `resolve_token` already
            // checked it at the call site); claim mode uses the
            // caller-supplied fields, which the call site already
            // ownership-checked with `require_customer`.
            let reference = match method {
                Some(method) => {
                    if method.customer_id != customer_id {
                        return Err(UnifiedError::forbidden(
                            "Payment method belongs to a different customer",
                        ));
                    }
                    CardMethodRef {
                        brand: method.brand,
                        last4: method.last4,
                        exp_month: method.exp_month,
                        exp_year: method.exp_year,
                    }
                }
                None => claimed
                    .expect("method or claim mode implies a method or claim was supplied"),
            };
            (
                None,
                Some(format!("******{}", reference.last4)),
                Some(reference.brand.clone()),
                reference.exp_month,
                reference.exp_year,
                false,
                0,
                false,
                Some(reference.last4.clone()),
                None,
            )
        };
    if !state.bus().has(CARDS_VALIDATE_REQUESTED) {
        return Err(cards_unavailable(masked_pan.as_deref()));
    }
    // The validator's wire shape: `number` (digits), `expiry`
    // (`"MM/YYYY"`, zero-padded month), `cvv` (the value, or null
    // when none was supplied). No settings ride along: the cards
    // module validates against its own per-tenant settings
    // (`PUT /api/v1/cards/settings`) and ignores any gateway-side
    // values, so the gateway sends none — the old
    // `test_mode` / `gpn_enabled` / `enabled_brands` /
    // `expiry_upper_bound_years` payload fields were dropped with
    // the gateway's `cards.*` settings namespace.
    let expiry = format!("{exp_month:02}/{exp_year}");
    let context = state.bus().emit(
        CARDS_VALIDATE_REQUESTED,
        json!({
            "mode": mode,
            "number": pan_digits,
            "expiry": expiry,
            "cvv": cvv_value,
            "masked_pan": masked_pan,
            "brand": brand,
            "cvv_present": cvv_present,
            "cvv_length": cvv_length,
            "cvv_digits_only": cvv_digits_only,
            "claimed_brand": brand,
            "claimed_last4": claimed_last4,
            "tenant_id": tenant.tenant_id.to_string(),
            "schema_name": tenant.schema_name,
            "customer_id": customer_id,
            "connection_id": connection.id.to_string(),
            "provider": connection.kind.as_str(),
            "order_id": order_id,
            "intent_id": intent_id.map(|id| id.to_string()),
        }),
    );
    // The cards module answers in two channels: `result` set on
    // the payload and the same object contributed via `add_result`.
    // Either may carry the boolean `valid`; the payload's `result`
    // wins when both are present. Silence on both channels is a
    // fail-closed `cards_unavailable`.
    let answered = context
        .results
        .iter()
        .find(|result| result.get("valid").and_then(Value::as_bool).is_some());
    let answer = context.get("result").or(answered);
    let valid = answer
        .and_then(|result| result.get("valid"))
        .and_then(Value::as_bool)
        .or_else(|| context.get("valid").and_then(Value::as_bool));
    let Some(valid) = valid else {
        return Err(cards_unavailable(masked_pan.as_deref()));
    };
    if !valid {
        // The validator reports generic codes in `reason_codes`
        // (first code wins — the validator emits exactly one per
        // failure). Its `missing_tenant_context` refusal is not a
        // validation reason: it maps to `cards_unavailable` (the
        // gate always supplies a tenant id, so it should never
        // fire). Any code outside the allowlist is a contract
        // violation and also maps to `cards_unavailable` — never
        // a guessed reason.
        let reason = answer
            .and_then(|result| result.get("reason_codes"))
            .and_then(Value::as_array)
            .and_then(|codes| codes.first())
            .and_then(Value::as_str)
            .unwrap_or(CARD_REASON_UNAVAILABLE);
        let reason = if reason == "missing_tenant_context" {
            CARD_REASON_UNAVAILABLE
        } else {
            reason
        };
        if !CARD_VALIDATION_REASONS.contains(&reason) {
            return Err(cards_unavailable(masked_pan.as_deref()));
        }
        let mut error = UnifiedError::unprocessable(format!("Card validation failed: {reason}"))
            .with_metadata("reason", Value::String(reason.to_string()))
            .with_metadata("mode", Value::String(mode.to_string()));
        if let Some(masked) = &masked_pan {
            error = error.with_metadata("masked_pan", Value::String(masked.clone()));
        }
        if let Some(answer_brand) = answer
            .and_then(|result| result.get("brand_name"))
            .or_else(|| answer.and_then(|result| result.get("brand_id")))
            .and_then(Value::as_str)
        {
            error = error.with_metadata("brand", Value::String(answer_brand.to_string()));
        }
        return Err(error);
    }
    Ok(Some(CardValidationOutcome {
        mode: mode.to_string(),
        brand: answer
            .and_then(|result| result.get("brand_name"))
            .or_else(|| answer.and_then(|result| result.get("brand_id")))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or(brand),
        masked_pan: answer
            .and_then(|result| result.get("masked_pan"))
            .and_then(Value::as_str)
            .filter(|masked| !masked.is_empty())
            .map(str::to_string)
            .or(masked_pan),
    }))
}

/// Cards settings endpoint: the capability names and the
/// validation contract for the card validation gate, plus
/// pointers to the canonical settings surfaces. Admin-only
/// (`cards:read`). The gateway keeps no `cards.*` /
/// `reference_id.*` settings of its own (see
/// `on_config_validate_cards`): card validation settings are
/// owned per tenant by the cards module
/// (`PUT /api/v1/cards/settings`), and reference-id settings are
/// owned by the store and billing modules
/// (`PUT /api/v1/store/reference-settings`,
/// `PUT /api/v1/billing/reference-settings`).
async fn get_cards_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_cards_capability(&ctx, &req, CARDS_READ)?;
        Ok(json!({
            "capabilities": {
                "read": CARDS_READ,
                "validate": CARDS_VALIDATE,
                "manage": CARDS_MANAGE,
            },
            "card_settings": "PUT /api/v1/cards/settings",
            "reference_settings": [
                "PUT /api/v1/store/reference-settings",
                "PUT /api/v1/billing/reference-settings",
            ],
            "validation_event": CARDS_VALIDATE_REQUESTED,
            "validation_reasons": CARD_VALIDATION_REASONS,
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Limits.
// ---------------------------------------------------------------------------

/// Default HTTP timeout for provider API calls.
const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// Default connect timeout for provider API calls.
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 10;
/// Longest `Retry-After` delay honoured from a provider.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(120);
/// User-Agent sent to provider APIs.
const USER_AGENT: &str = concat!("unified/", env!("CARGO_PKG_VERSION"), " payments");
/// Maximum amount in minor units accepted for one charge (sanity guard).
const MAX_AMOUNT_MINOR: i64 = 1_000_000_000_000;
/// Webhook event ids are remembered for this long (duplicate suppression).
const WEBHOOK_EVENT_TTL_SECS: i64 = 86_400;

/// Maximum age of a Stripe webhook signature timestamp, in seconds.
/// Stripe signs `timestamp.body`; without a window a captured signed
/// payload stays replayable forever, so timestamps older than this
/// are rejected even when the signature itself verifies.
const STRIPE_WEBHOOK_TOLERANCE_SECS: i64 = 300;

// ---------------------------------------------------------------------------
// Provider kinds, capabilities, currencies, settings fields.
// ---------------------------------------------------------------------------

/// Every payment provider this module can talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

fn field(
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

// ---------------------------------------------------------------------------
// Shared result shapes.
// ---------------------------------------------------------------------------

/// Normalized payment status, shared by every adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

// ---------------------------------------------------------------------------
// Requests (adapter input).
// ---------------------------------------------------------------------------

/// Billing details supplied by the shopper. Only 2Checkout requires
/// them (an order cannot be placed without billing details); every
/// other provider ignores them.
#[derive(Debug, Clone, Default)]
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
    /// `crate::core::reference_id`). Threaded from the store order's
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

// ---------------------------------------------------------------------------
// Batch 18: payment reference ids (BEGIN — append-only section)
// ---------------------------------------------------------------------------
//
// The provider-facing reference for every charge, payout, refund,
// and subscription name in this module is a reference in the single
// shared format (`PREFIX-12hex-12hex`, 30 characters; see
// `crate::core::reference_id`). The store order id (a UUID) is NEVER
// sent to a provider as the order reference: the provider sees only
// the 30-character reference, while the store link stays in
// `ChargeInput::order_id` and on the recorded transaction, so
// reconciliation never loses the order. The reference for a charge
// resolves in this order: the reference threaded on the input, the
// order id when it already is a valid reference, otherwise a freshly
// generated reference. Nothing here invents a second format: the
// `pay-{uuid}`, `ord-{uuid}`, `txn-{uuid}`, `payout-{uuid}`,
// `subscription-{uuid}`, `bt-{uuid}`, and `bt-refund-{uuid}`
// fallbacks are all replaced by this one helper.

/// Resolve the provider-facing reference for a charge: the threaded
/// `reference_id` when it is a valid reference, otherwise the order
/// id when it already is one, otherwise a freshly generated
/// reference.
fn charge_reference(input: &ChargeInput) -> String {
    if let Some(reference) = input
        .reference_id
        .as_deref()
        .filter(|reference| reference_id::is_valid(reference))
    {
        return reference.to_string();
    }
    provider_reference(input.order_id.as_deref())
}

/// Resolve a provider-facing reference from an order id alone: the
/// id itself when it already is a valid reference, otherwise a
/// freshly generated reference with the default prefix.
fn provider_reference(order_id: Option<&str>) -> String {
    match order_id.filter(|id| reference_id::is_valid(id)) {
        Some(id) => id.to_string(),
        None => reference_id::generate(None),
    }
}

/// The store order's reference for a pending checkout, when the
/// store recorded one through `payment.initiated`. The pending
/// checkout payload carries the order JSON, which includes the
/// `reference` issued at checkout. Returns `None` when there is no
/// pending checkout for the order or its reference is not valid.
fn pending_checkout_reference(order_id: Option<&str>) -> Option<String> {
    let order_id = order_id?;
    let record = pending_checkouts_store().get(order_id)?;
    let reference = record
        .get("order")?
        .get("reference")?
        .as_str()?;
    if reference_id::is_valid(reference) {
        Some(reference.to_string())
    } else {
        None
    }
}

// Batch 18: payment reference ids (END)
// ---------------------------------------------------------------------------

/// Input to an adapter refund call.
#[derive(Debug, Clone)]
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

// ---------------------------------------------------------------------------
// Credentials and connections.
// ---------------------------------------------------------------------------

/// Credentials for one connection. Every field is optional at the type
/// level; `validate_connection` enforces the per-kind required set.
/// Values are write-only: they are never serialized into a view, never
/// logged, and never included in hook payloads.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PaymentCredentials {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub secret_key: Option<String>,
    #[serde(default)]
    pub public_key: Option<String>,
    #[serde(default)]
    pub merchant_id: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub bearer_token: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub webhook_secret: Option<String>,
    #[serde(default)]
    pub webhook_id: Option<String>,
    #[serde(default)]
    pub callback_token: Option<String>,
    #[serde(default)]
    pub server_key: Option<String>,
    #[serde(default)]
    pub client_key: Option<String>,
    #[serde(default)]
    pub secret_word: Option<String>,
}

impl PaymentCredentials {
    /// Names of the credential fields that are set (non-empty), for the
    /// masked read view. Values are never exposed.
    fn configured_fields(&self) -> Vec<&'static str> {
        let mut fields = Vec::new();
        let present = |value: &Option<String>| {
            value.as_deref().map(str::is_empty).unwrap_or(true) == false
        };
        if present(&self.api_key) {
            fields.push("api_key");
        }
        if present(&self.secret_key) {
            fields.push("secret_key");
        }
        if present(&self.public_key) {
            fields.push("public_key");
        }
        if present(&self.merchant_id) {
            fields.push("merchant_id");
        }
        if present(&self.client_id) {
            fields.push("client_id");
        }
        if present(&self.client_secret) {
            fields.push("client_secret");
        }
        if present(&self.access_token) {
            fields.push("access_token");
        }
        if present(&self.bearer_token) {
            fields.push("bearer_token");
        }
        if present(&self.username) {
            fields.push("username");
        }
        if present(&self.password) {
            fields.push("password");
        }
        if present(&self.webhook_secret) {
            fields.push("webhook_secret");
        }
        if present(&self.webhook_id) {
            fields.push("webhook_id");
        }
        if present(&self.callback_token) {
            fields.push("callback_token");
        }
        if present(&self.server_key) {
            fields.push("server_key");
        }
        if present(&self.client_key) {
            fields.push("client_key");
        }
        if present(&self.secret_word) {
            fields.push("secret_word");
        }
        fields
    }

    /// Merge an update into these credentials: only fields present in
    /// the update overwrite the stored value, so a partial update
    /// never wipes credentials the caller did not resend.
    fn merge(&mut self, update: PaymentCredentials) {
        if update.api_key.is_some() {
            self.api_key = update.api_key;
        }
        if update.secret_key.is_some() {
            self.secret_key = update.secret_key;
        }
        if update.public_key.is_some() {
            self.public_key = update.public_key;
        }
        if update.merchant_id.is_some() {
            self.merchant_id = update.merchant_id;
        }
        if update.client_id.is_some() {
            self.client_id = update.client_id;
        }
        if update.client_secret.is_some() {
            self.client_secret = update.client_secret;
        }
        if update.access_token.is_some() {
            self.access_token = update.access_token;
        }
        if update.bearer_token.is_some() {
            self.bearer_token = update.bearer_token;
        }
        if update.username.is_some() {
            self.username = update.username;
        }
        if update.password.is_some() {
            self.password = update.password;
        }
        if update.webhook_secret.is_some() {
            self.webhook_secret = update.webhook_secret;
        }
        if update.webhook_id.is_some() {
            self.webhook_id = update.webhook_id;
        }
        if update.callback_token.is_some() {
            self.callback_token = update.callback_token;
        }
        if update.server_key.is_some() {
            self.server_key = update.server_key;
        }
        if update.client_key.is_some() {
            self.client_key = update.client_key;
        }
        if update.secret_word.is_some() {
            self.secret_word = update.secret_word;
        }
    }
}

/// Sandbox or live mode for a connection. Credentials are per mode in
/// practice: switching mode does not rewrite credentials, it only
/// changes which base URL the adapter uses, so the operator must store
/// the matching credentials for the active mode. The mode endpoint
/// exists so the switch is explicit and auditable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayMode {
    Sandbox,
    Live,
}

impl GatewayMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Sandbox => "sandbox",
            Self::Live => "live",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "sandbox" | "test" => Some(Self::Sandbox),
            "live" | "production" => Some(Self::Live),
            _ => None,
        }
    }
}

/// Config-driven definition for `PaymentProviderKind::Custom` (and the
/// endpoint contract for `PaymentProviderKind::Card`). Contains no
/// credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomGatewayConfig {
    /// `none`, `bearer`, `basic`, `api_key_header`, `api_key_query`,
    /// or `hmac_sha256`.
    pub auth_type: String,
    /// Path of the charge endpoint, for example `/v1/charges`.
    pub charge_path: String,
    #[serde(default)]
    pub authorize_path: Option<String>,
    #[serde(default)]
    pub capture_path: Option<String>,
    #[serde(default)]
    pub refund_path: Option<String>,
    #[serde(default)]
    pub void_path: Option<String>,
    #[serde(default)]
    pub subscription_path: Option<String>,
    #[serde(default)]
    pub payout_path: Option<String>,
    #[serde(default)]
    pub test_path: Option<String>,
    /// Header carrying the webhook signature, default `X-Signature`.
    #[serde(default)]
    pub signature_header: Option<String>,
    /// JSON field holding the transaction id in responses,
    /// default `id`.
    #[serde(default)]
    pub transaction_id_field: Option<String>,
    /// JSON field holding the status in responses, default `status`.
    #[serde(default)]
    pub status_field: Option<String>,
    /// Capabilities this custom gateway actually offers. A missing
    /// endpoint path disables the matching capability.
    #[serde(default)]
    pub supports_authorize: bool,
    #[serde(default)]
    pub supports_capture: bool,
    #[serde(default = "default_true")]
    pub supports_refund: bool,
    #[serde(default)]
    pub supports_void: bool,
    #[serde(default)]
    pub supports_subscriptions: bool,
    #[serde(default)]
    pub supports_payouts: bool,
}

impl Default for CustomGatewayConfig {
    fn default() -> Self {
        Self {
            auth_type: "bearer".to_string(),
            charge_path: "/v1/charges".to_string(),
            authorize_path: None,
            capture_path: None,
            refund_path: None,
            void_path: None,
            subscription_path: None,
            payout_path: None,
            test_path: None,
            signature_header: None,
            transaction_id_field: None,
            status_field: None,
            supports_authorize: false,
            supports_capture: false,
            supports_refund: true,
            supports_void: false,
            supports_subscriptions: false,
            supports_payouts: false,
        }
    }
}

fn default_true() -> bool {
    true
}

impl CustomGatewayConfig {
    fn capabilities(&self) -> PaymentCapabilities {
        PaymentCapabilities {
            supports_authorize: self.supports_authorize && self.authorize_path.is_some(),
            supports_capture: self.supports_capture && self.capture_path.is_some(),
            supports_charge: true,
            supports_refund: self.supports_refund && self.refund_path.is_some(),
            supports_void: self.supports_void && self.void_path.is_some(),
            supports_subscriptions: self.supports_subscriptions
                && self.subscription_path.is_some(),
            supports_payouts: self.supports_payouts && self.payout_path.is_some(),
            supports_3ds: false,
            supports_webhooks: true,
            subscriptions_are_local_schedule: false,
        }
    }
}

/// One stored provider connection. Kept in memory (same pattern as the
/// provider registry and the store); durable persistence is a
/// follow-up for the integrator — see the migration sketch in the
/// coordinator report.
#[derive(Debug, Clone)]
pub struct PaymentConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: PaymentProviderKind,
    pub mode: GatewayMode,
    /// API / processor endpoint for Card, Mastercard, and Custom.
    /// Empty for fixed-host providers.
    pub endpoint: Option<String>,
    pub credentials: PaymentCredentials,
    /// Config-driven definition for `PaymentProviderKind::Custom`
    /// (`None` for every other kind).
    pub custom_config: Option<CustomGatewayConfig>,
    /// Wallet adapters only: the gateway connection that performs the
    /// charge for a wallet token.
    pub delegate_connection_id: Option<Uuid>,
    pub enabled: bool,
    pub last_tested_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Masked read view of a connection: credentials are never returned.
#[derive(Debug, Clone, Serialize)]
pub struct PaymentConnectionView {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub kind_display: String,
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Names of credential fields that are configured (values hidden).
    pub configured_credentials: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_config: Option<CustomGatewayConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delegate_connection_id: Option<Uuid>,
    pub capabilities: PaymentCapabilities,
    pub currencies: Vec<String>,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_tested_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl PaymentConnection {
    fn view(&self) -> PaymentConnectionView {
        let capabilities = match (self.kind, &self.custom_config) {
            (PaymentProviderKind::Custom, Some(config)) => config.capabilities(),
            _ => capabilities_for(self.kind),
        };
        PaymentConnectionView {
            id: self.id,
            name: self.name.clone(),
            kind: self.kind.as_str().to_string(),
            kind_display: self.kind.display_name().to_string(),
            mode: self.mode.as_str().to_string(),
            endpoint: self.endpoint.clone(),
            configured_credentials: self
                .credentials
                .configured_fields()
                .into_iter()
                .map(str::to_string)
                .collect(),
            custom_config: self.custom_config.clone(),
            delegate_connection_id: self.delegate_connection_id,
            capabilities,
            currencies: currencies_for(self.kind)
                .iter()
                .map(|currency| currency.to_string())
                .collect(),
            enabled: self.enabled,
            last_tested_at: self.last_tested_at,
            last_status: self.last_status.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Connection validation.
// ---------------------------------------------------------------------------

/// Validate a user-supplied API endpoint: https only (money must never
/// travel over plain http), a host, no embedded credentials. Private /
/// loopback hosts are refused here — a payment processor endpoint is a
/// public service, never localhost.
fn validate_payment_endpoint(endpoint: Option<&str>, kind: &str) -> UnifiedResult<Url> {
    let Some(raw) = endpoint else {
        return Err(UnifiedError::bad_request(format!(
            "Provider \"{kind}\" requires endpoint (the processor / gateway API base URL)"
        )));
    };
    let url = Url::parse(raw.trim())
        .map_err(|_| UnifiedError::bad_request(format!("Invalid endpoint URL \"{raw}\"")))?;
    if url.scheme() != "https" {
        return Err(UnifiedError::bad_request(
            "Payment endpoint URL must use https",
        ));
    }
    let host = url.host_str().unwrap_or("");
    if host.is_empty() {
        return Err(UnifiedError::bad_request(
            "Endpoint URL must include a host",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "Endpoint URL must not embed credentials; use the credential fields",
        ));
    }
    if host == "localhost" || host.ends_with(".localhost") || host == "127.0.0.1" || host == "[::1]" {
        return Err(UnifiedError::bad_request(
            "Payment endpoint must not point at localhost",
        ));
    }
    Ok(url)
}

/// Validate a Mastercard gateway host: it must be a `*.mastercard.com`
/// host (the merchant gateway host supplied by Mastercard).
fn validate_mastercard_endpoint(endpoint: &str) -> UnifiedResult<()> {
    let url = validate_payment_endpoint(Some(endpoint), "mastercard")?;
    let host = url.host_str().unwrap_or("");
    if host != "mastercard.com" && !host.ends_with(".mastercard.com") {
        return Err(UnifiedError::bad_request(format!(
            "Mastercard gateway host \"{host}\" must be a *.mastercard.com host"
        )));
    }
    Ok(())
}

/// Validate a custom gateway config: known auth type, paths that start
/// with `/`, and no path that tries to escape the base URL.
fn validate_custom_config(config: &CustomGatewayConfig) -> UnifiedResult<()> {
    match config.auth_type.as_str() {
        "none" | "bearer" | "basic" | "api_key_header" | "api_key_query" | "hmac_sha256" => {}
        other => {
            return Err(UnifiedError::bad_request(format!(
                "Unknown custom auth type \"{other}\" (expected none, bearer, basic, api_key_header, api_key_query, or hmac_sha256)"
            )));
        }
    }
    let mut paths: Vec<&str> = vec![config.charge_path.as_str()];
    for path in [
        &config.authorize_path,
        &config.capture_path,
        &config.refund_path,
        &config.void_path,
        &config.subscription_path,
        &config.payout_path,
        &config.test_path,
    ] {
        if let Some(path) = path {
            paths.push(path.as_str());
        }
    }
    for path in paths {
        if !path.starts_with('/') {
            return Err(UnifiedError::bad_request(format!(
                "Custom endpoint path \"{path}\" must start with '/'"
            )));
        }
        if path.contains("..") {
            return Err(UnifiedError::bad_request(format!(
                "Custom endpoint path \"{path}\" must not contain '..'"
            )));
        }
    }
    if config.supports_subscriptions && config.subscription_path.is_none() {
        return Err(UnifiedError::bad_request(
            "Custom provider with supports_subscriptions needs subscription_path",
        ));
    }
    if config.supports_payouts && config.payout_path.is_none() {
        return Err(UnifiedError::bad_request(
            "Custom provider with supports_payouts needs payout_path",
        ));
    }
    Ok(())
}

/// Required credential / field set per kind. A bad connection is
/// rejected at creation time instead of failing its first API call.
fn validate_connection(input: &CreateConnectionRequest) -> UnifiedResult<PaymentProviderKind> {
    let kind = PaymentProviderKind::from_str(&input.kind).ok_or_else(|| {
        UnifiedError::bad_request(format!("Unknown payment provider kind \"{}\"", input.kind))
    })?;
    if input.name.trim().is_empty() {
        return Err(UnifiedError::bad_request("Connection name is required"));
    }
    let mode = match &input.mode {
        None => GatewayMode::Sandbox,
        Some(mode) => GatewayMode::from_str(mode).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown gateway mode \"{mode}\" (expected sandbox or live)"
            ))
        })?,
    };
    let _ = mode;
    let creds = &input.credentials;
    let has = |value: &Option<String>| {
        value.as_deref().map(|v| !v.trim().is_empty()).unwrap_or(false)
    };
    let missing = |field: &str| {
        UnifiedError::bad_request(format!(
            "Provider \"{}\" requires credential field \"{field}\"",
            kind.as_str()
        ))
    };
    match kind {
        PaymentProviderKind::Xendit => {
            if !has(&creds.api_key) {
                return Err(missing("api_key"));
            }
            if !has(&creds.callback_token) {
                return Err(missing("callback_token"));
            }
        }
        PaymentProviderKind::HyperPay => {
            if !has(&creds.access_token) {
                return Err(missing("access_token"));
            }
            if !has(&creds.merchant_id) {
                return Err(missing("merchant_id"));
            }
        }
        PaymentProviderKind::TwoCheckout => {
            if !has(&creds.merchant_id) {
                return Err(missing("merchant_id"));
            }
            if !has(&creds.secret_word) && !has(&creds.api_key) {
                return Err(UnifiedError::bad_request(
                    "Provider \"2checkout\" requires secret_word (or api_key)",
                ));
            }
        }
        PaymentProviderKind::Card => {
            if !has(&creds.api_key) {
                return Err(missing("api_key"));
            }
            validate_payment_endpoint(input.endpoint.as_deref(), "card")?;
        }
        PaymentProviderKind::Mastercard => {
            if !has(&creds.merchant_id) {
                return Err(missing("merchant_id"));
            }
            if !has(&creds.secret_key) {
                return Err(missing("secret_key"));
            }
            let endpoint = input.endpoint.as_deref().ok_or_else(|| {
                UnifiedError::bad_request(
                    "Provider \"mastercard\" requires endpoint (the merchant gateway host)",
                )
            })?;
            validate_mastercard_endpoint(endpoint)?;
        }
        PaymentProviderKind::GooglePay | PaymentProviderKind::ApplePay => {
            if input.delegate_connection_id.is_none() {
                return Err(UnifiedError::bad_request(format!(
                    "Provider \"{}\" requires delegate_connection_id (the gateway connection that performs the charge)",
                    kind.as_str()
                )));
            }
            let delegate_id = input.delegate_connection_id.unwrap();
            let Some(delegate) = connections_store().get(&delegate_id) else {
                return Err(UnifiedError::bad_request(format!(
                    "Delegate connection \"{delegate_id}\" does not exist"
                )));
            };
            if delegate.kind.needs_delegate() {
                return Err(UnifiedError::bad_request(
                    "A wallet provider cannot delegate to another wallet provider",
                ));
            }
            if !delegate.enabled {
                return Err(UnifiedError::bad_request(
                    "Delegate connection is disabled",
                ));
            }
        }
        PaymentProviderKind::PayPal => {
            if !has(&creds.client_id) {
                return Err(missing("client_id"));
            }
            if !has(&creds.client_secret) {
                return Err(missing("client_secret"));
            }
            if !has(&creds.webhook_id) {
                return Err(missing("webhook_id"));
            }
        }
        PaymentProviderKind::Stripe => {
            if !has(&creds.secret_key) {
                return Err(missing("secret_key"));
            }
            if !has(&creds.webhook_secret) {
                return Err(missing("webhook_secret"));
            }
        }
        PaymentProviderKind::Midtrans => {
            if !has(&creds.server_key) {
                return Err(missing("server_key"));
            }
        }
        PaymentProviderKind::BankTransfer => {
            // No credentials and no API endpoint: bank accounts are
            // configured in the bank-transfer module, not here.
            if let Some(endpoint) = input.endpoint.as_deref() {
                if !endpoint.trim().is_empty() {
                    return Err(UnifiedError::bad_request(
                        "Provider \"bank_transfer\" does not use an API endpoint; bank accounts are configured in the bank-transfer module",
                    ));
                }
            }
        }
        PaymentProviderKind::Custom => {
            validate_payment_endpoint(input.endpoint.as_deref(), "custom")?;
            let Some(config) = &input.custom_config else {
                return Err(UnifiedError::bad_request(
                    "Provider \"custom\" requires custom_config (auth type and endpoint paths)",
                ));
            };
            validate_custom_config(config)?;
        }
    }
    if let Some(endpoint) = input.endpoint.as_deref() {
        if kind.has_fixed_hosts() {
            let url = Url::parse(endpoint.trim()).map_err(|_| {
                UnifiedError::bad_request(format!("Invalid endpoint URL \"{endpoint}\""))
            })?;
            let host = url.host_str().unwrap_or("");
            if !kind.allowed_hosts().contains(&host) {
                return Err(UnifiedError::bad_request(format!(
                    "Endpoint host \"{host}\" is not an allowed {} API host",
                    kind.as_str()
                )));
            }
        }
    }
    Ok(kind)
}

// ---------------------------------------------------------------------------
// Records: intents, transactions, payment methods, subscriptions.
// ---------------------------------------------------------------------------

/// A payment intent: the record a checkout creates first, then
/// confirms. The intent holds no card data — only a token reference.
#[derive(Debug, Clone, Serialize)]
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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Split bills: one order/invoice total divided across methods,
// payers, or instalments.
// ---------------------------------------------------------------------------

/// One part of a split bill. Amounts are integer minor units; the
/// provider is a registry kind string (`stripe`, `bank_transfer`,
/// ...). A part is paid through the gateway like any other charge,
/// except a `bank_transfer` part, which opens a manual verification
/// in the bank-transfer module instead of charging.
#[derive(Debug, Clone, Serialize)]
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

static CONNECTIONS: OnceLock<DashMap<Uuid, PaymentConnection>> = OnceLock::new();

fn connections_store() -> &'static DashMap<Uuid, PaymentConnection> {
    CONNECTIONS.get_or_init(DashMap::new)
}

static INTENTS: OnceLock<DashMap<Uuid, PaymentIntent>> = OnceLock::new();

fn intents_store() -> &'static DashMap<Uuid, PaymentIntent> {
    INTENTS.get_or_init(DashMap::new)
}

static TRANSACTIONS: OnceLock<DashMap<Uuid, PaymentTransaction>> = OnceLock::new();

fn transactions_store() -> &'static DashMap<Uuid, PaymentTransaction> {
    TRANSACTIONS.get_or_init(DashMap::new)
}

static METHODS: OnceLock<DashMap<Uuid, PaymentMethod>> = OnceLock::new();

fn methods_store() -> &'static DashMap<Uuid, PaymentMethod> {
    METHODS.get_or_init(DashMap::new)
}

static SUBSCRIPTIONS: OnceLock<DashMap<Uuid, GatewaySubscription>> = OnceLock::new();

fn subscriptions_store() -> &'static DashMap<Uuid, GatewaySubscription> {
    SUBSCRIPTIONS.get_or_init(DashMap::new)
}

static SPLIT_BILLS: OnceLock<DashMap<Uuid, SplitBill>> = OnceLock::new();

fn splits_store() -> &'static DashMap<Uuid, SplitBill> {
    SPLIT_BILLS.get_or_init(DashMap::new)
}

/// Idempotency index: (connection id, idempotency key) -> transaction
/// id. A repeated mutating call with the same key returns the original
/// transaction instead of charging twice.
static IDEMPOTENCY: OnceLock<DashMap<(Uuid, String), Uuid>> = OnceLock::new();

fn idempotency_store() -> &'static DashMap<(Uuid, String), Uuid> {
    IDEMPOTENCY.get_or_init(DashMap::new)
}

/// Processed webhook event ids, with the time they were first seen, so
/// a redelivered webhook is acknowledged but not processed twice.
static WEBHOOK_EVENTS: OnceLock<DashMap<String, DateTime<Utc>>> = OnceLock::new();

/// Drop remembered webhook event ids older than the TTL, so the
/// duplicate-suppression store stays bounded.
fn prune_webhook_events() {
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

fn webhook_events_store() -> &'static DashMap<String, DateTime<Utc>> {
    WEBHOOK_EVENTS.get_or_init(DashMap::new)
}

/// Checkouts started through the store's `payment.initiated` hook,
/// keyed by order id, so the gateway can answer a later confirm
/// without the caller resending the order.
static PENDING_CHECKOUTS: OnceLock<DashMap<String, Value>> = OnceLock::new();

fn pending_checkouts_store() -> &'static DashMap<String, Value> {
    PENDING_CHECKOUTS.get_or_init(DashMap::new)
}

/// Cached PayPal OAuth2 access tokens per connection, with expiry.
static PAYPAL_TOKENS: OnceLock<DashMap<Uuid, (String, DateTime<Utc>)>> = OnceLock::new();

fn paypal_tokens_store() -> &'static DashMap<Uuid, (String, DateTime<Utc>)> {
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
    factories: RwLock<HashMap<PaymentProviderKind, PaymentProviderFactory>>,
}

impl PaymentRegistry {
    fn new() -> Self {
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

static REGISTRY: OnceLock<PaymentRegistry> = OnceLock::new();

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

// ---------------------------------------------------------------------------
// Common adapter interface.
// ---------------------------------------------------------------------------

/// The interface every payment adapter implements. Only `charge` is
/// required in the sense that every kind supports it; every other
/// operation has a default implementation that fails with a clear
/// "not supported" error naming the missing capability, so a
/// charge-only adapter only implements `kind`, `charge`,
/// `test_connection`, and `webhook_verify`.
#[async_trait]
pub trait PaymentProvider: Send + Sync {
    fn kind(&self) -> PaymentProviderKind;

    fn capabilities(&self) -> PaymentCapabilities {
        capabilities_for(self.kind())
    }

    /// Verify the credentials against the real provider API.
    async fn test_connection(&self) -> UnifiedResult<ConnectionTestResult>;

    /// Charge a tokenized payment method immediately.
    async fn charge(&self, input: &ChargeInput) -> UnifiedResult<ChargeResult>;

    /// Authorize (hold funds without capturing). Guarded by
    /// `supports_authorize`.
    async fn authorize(&self, _input: &ChargeInput) -> UnifiedResult<ChargeResult> {
        Err(unsupported(self.kind(), "authorize", "supports_authorize"))
    }

    /// Capture a previous authorization. Guarded by
    /// `supports_capture`.
    async fn capture(
        &self,
        _provider_transaction_id: &str,
        _amount_minor: Option<i64>,
    ) -> UnifiedResult<ChargeResult> {
        Err(unsupported(self.kind(), "capture", "supports_capture"))
    }

    /// Refund a captured charge, fully or partially. Guarded by
    /// `supports_refund`.
    async fn refund(&self, _input: &RefundInput) -> UnifiedResult<RefundResult> {
        Err(unsupported(self.kind(), "refund", "supports_refund"))
    }

    /// Void an authorization (or an uncaptured charge). Guarded by
    /// `supports_void`.
    async fn void(&self, _provider_transaction_id: &str) -> UnifiedResult<()> {
        Err(unsupported(self.kind(), "void", "supports_void"))
    }

    /// Create a subscription. Guarded by `supports_subscriptions`.
    async fn subscription_create(
        &self,
        _input: &SubscriptionInput,
    ) -> UnifiedResult<SubscriptionResult> {
        Err(unsupported(
            self.kind(),
            "subscription_create",
            "supports_subscriptions",
        ))
    }

    /// Cancel a subscription. Guarded by `supports_subscriptions`.
    async fn subscription_cancel(&self, _provider_subscription_id: &str) -> UnifiedResult<()> {
        Err(unsupported(
            self.kind(),
            "subscription_cancel",
            "supports_subscriptions",
        ))
    }

    /// Verify a webhook's signature before any processing. Returns
    /// `Ok(true)` when the signature is valid, `Ok(false)` when the
    /// provider supplies a signature that does not match, and an
    /// error when verification cannot be performed at all (missing
    /// secret, unsupported provider, or a failed server-side lookup).
    /// A `false` or an error both mean the webhook must be rejected.
    async fn webhook_verify(
        &self,
        headers: &HeaderMap,
        raw_body: &[u8],
    ) -> UnifiedResult<bool>;

    /// Server-side outcome of the event a webhook claims, for
    /// providers that do not sign their webhooks and are verified by
    /// re-fetching the resource (HyperPay, 2Checkout). `Ok(None)`
    /// means the provider signs its webhooks, so the verified body is
    /// authoritative. `Ok(Some(status))` means the receiver must apply
    /// the fetched status, not the body's claim: for these providers
    /// the body is unauthenticated, so trusting its event type would
    /// let anyone with a real checkout / order id forge a capture.
    /// An error means the outcome could not be confirmed and the
    /// webhook must not be applied.
    async fn webhook_outcome(&self, _raw_body: &[u8]) -> UnifiedResult<Option<PaymentStatus>> {
        Ok(None)
    }

    /// Pay money out (disbursement / transfer / payout). Guarded by
    /// `supports_payouts`.
    async fn payout(&self, _input: &PayoutInput) -> UnifiedResult<PayoutResult> {
        Err(unsupported(self.kind(), "payout", "supports_payouts"))
    }
}

fn unsupported(kind: PaymentProviderKind, action: &str, capability: &str) -> UnifiedError {
    UnifiedError::bad_request(format!(
        "Provider \"{}\" does not support {action} (capability {capability} is false)",
        kind.as_str()
    ))
}

// ---------------------------------------------------------------------------
// Shared HTTP helpers.
// ---------------------------------------------------------------------------

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn sha512_hex(data: &[u8]) -> String {
    hex_encode(&Sha512::digest(data))
}

type HmacSha256 = Hmac<Sha256>;

fn hmac_sha256_hex(key: &[u8], message: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key length is always valid");
    mac.update(message);
    hex_encode(&mac.finalize().into_bytes())
}

/// Constant-time-ish equality for signatures (length check first, then
/// a full scan so the timing does not leak the mismatch position).
fn digests_equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn build_client() -> UnifiedResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS))
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))
}

/// Refuse a URL whose host is not on the provider's allow-list. Called
/// before every request to a fixed-host provider.
fn check_allowlist(url: &Url, kind: PaymentProviderKind) -> UnifiedResult<()> {
    if !kind.has_fixed_hosts() {
        return Ok(());
    }
    let host = url.host_str().unwrap_or("");
    if !kind.allowed_hosts().contains(&host) {
        return Err(UnifiedError::bad_request(format!(
            "Refusing request to non-allow-listed host \"{host}\" for provider \"{}\"",
            kind.as_str()
        )));
    }
    Ok(())
}

/// Parse a `Retry-After` header (seconds only; HTTP dates are ignored),
/// capped so a misbehaving provider cannot stall a caller indefinitely.
fn parse_retry_after(response: &reqwest::Response) -> Option<Duration> {
    let value = response.headers().get(RETRY_AFTER)?.to_str().ok()?;
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(|secs| Duration::from_secs(secs).min(MAX_RETRY_AFTER))
}

/// Map a provider HTTP failure into the standard error envelope. The
/// client-visible message stays generic; the provider's own error code
/// (when the body carries one) goes into `metadata.provider_code`, and
/// a 429 keeps its status with `retry_after_secs`. Provider response
/// bodies are never copied into the message.
fn map_provider_error(
    status: StatusCode,
    provider: &str,
    body: &Value,
    retry_after: Option<Duration>,
) -> UnifiedError {
    let provider_code = body
        .get("error")
        .and_then(|error| error.get("code").or_else(|| error.get("type")))
        .and_then(|code| code.as_str())
        .or_else(|| body.get("code").and_then(|code| code.as_str()))
        .or_else(|| body.get("error_code").and_then(|code| code.as_str()))
        .map(str::to_string);
    let message = format!("Provider \"{provider}\" request failed with HTTP {status}");
    let mut error = match status {
        StatusCode::TOO_MANY_REQUESTS => UnifiedError::new(message, 429),
        StatusCode::UNAUTHORIZED => UnifiedError::unauthorized(format!(
            "Provider \"{provider}\" rejected the credentials (HTTP 401)"
        )),
        StatusCode::FORBIDDEN => UnifiedError::forbidden(format!(
            "Provider \"{provider}\" refused the request (HTTP 403): the credentials lack permission for this operation"
        )),
        StatusCode::NOT_FOUND => {
            UnifiedError::not_found(format!("Provider \"{provider}\" resource not found (HTTP 404)"))
        }
        _ if status.is_client_error() => UnifiedError::bad_request(message),
        _ => UnifiedError::internal(message),
    };
    error = error.with_metadata("provider", Value::String(provider.to_string()));
    if let Some(code) = provider_code {
        error = error.with_metadata("provider_code", Value::String(code));
    }
    if let Some(delay) = retry_after {
        error = error.with_metadata("retry_after_secs", Value::from(delay.as_secs()));
    }
    error
}

/// Send a request and return the parsed JSON body, or a mapped error.
/// A 429 is returned as an error (with `retry_after_secs` in metadata)
/// instead of being retried here, so the caller decides.
async fn send_json(
    request: reqwest::RequestBuilder,
    provider: &str,
) -> UnifiedResult<Value> {
    let response = request.send().await.map_err(|error| {
        UnifiedError::internal(format!("Provider \"{provider}\" request failed: {error}"))
    })?;
    let status = response.status();
    let retry_after = parse_retry_after(&response);
    if !status.is_success() {
        let body: Value = response.json().await.unwrap_or(Value::Null);
        return Err(map_provider_error(status, provider, &body, retry_after));
    }
    response.json::<Value>().await.map_err(|error| {
        UnifiedError::internal(format!(
            "Provider \"{provider}\" returned an unreadable response: {error}"
        ))
    })
}

/// Send a request and return the raw response (status + best-effort
/// JSON body) without mapping the status to an error. Used by
/// connection tests that need to distinguish 401 (bad credentials)
/// from 404 (credentials accepted, resource absent).
async fn call_raw(
    request: reqwest::RequestBuilder,
    provider: &str,
) -> UnifiedResult<(StatusCode, Value)> {
    let response = request.send().await.map_err(|error| {
        UnifiedError::internal(format!("Provider \"{provider}\" request failed: {error}"))
    })?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    Ok((status, body))
}

/// Read the first present string field from a JSON body.
fn json_str<'a>(body: &'a Value, keys: &[&str]) -> Option<&'a str> {
    for key in keys {
        if let Some(value) = body.get(key).and_then(|value| value.as_str()) {
            return Some(value);
        }
    }
    None
}

/// Read the first present integer field from a JSON body.
fn json_i64(body: &Value, keys: &[&str]) -> Option<i64> {
    for key in keys {
        if let Some(value) = body.get(key).and_then(|value| value.as_i64()) {
            return Some(value);
        }
    }
    None
}

/// Read a fee from the common fee field names, when the provider
/// reports one. Fees are never estimated: no field means no fee.
fn fee_from(body: &Value) -> Option<i64> {
    json_i64(
        body,
        &["fee", "fee_amount", "processing_fee", "application_fee_amount"],
    )
}

/// Currencies with no minor unit (amounts are already whole units).
fn is_zero_decimal(currency: &str) -> bool {
    matches!(currency, "JPY" | "KRW" | "VND" | "CLP")
}

/// Convert a major-unit amount (a store order total) to integer
/// minor units, mirroring the store's conversion.
fn major_to_minor(amount_major: f64, currency: &str) -> i64 {
    if is_zero_decimal(currency) {
        amount_major.round() as i64
    } else {
        (amount_major * 100.0).round() as i64
    }
}

/// Amount-tampering guard for client-supplied amounts: when the
/// order went through the store checkout, the store recorded the
/// order total in `payment.initiated` (kept in
/// `pending_checkouts_store`). A charge or intent for that order
/// must carry exactly that total in that currency; anything else
/// is a tampered amount. Orders with no recorded checkout (direct
/// API charges) have nothing to compare against, so they pass —
/// the caller is trusted there, which is why this is a second line
/// of defence and not a substitute for API authentication.
fn check_pending_checkout_amount(
    order_id: Option<&str>,
    amount_minor: i64,
    currency: &str,
) -> UnifiedResult<()> {
    let Some(order_id) = order_id.filter(|id| !id.is_empty()) else {
        return Ok(());
    };
    let Some(record) = pending_checkouts_store().get(order_id) else {
        return Ok(());
    };
    let order = record.value().get("order");
    let total = order
        .and_then(|order| order.get("total"))
        .and_then(|value| value.as_f64());
    let order_currency = order
        .and_then(|order| order.get("currency"))
        .and_then(|value| value.as_str());
    let (Some(total), Some(order_currency)) = (total, order_currency) else {
        return Err(UnifiedError::unprocessable(
            "Checkout record for this order has no verifiable total",
        ));
    };
    if !currency.eq_ignore_ascii_case(order_currency)
        || amount_minor != major_to_minor(total, order_currency)
    {
        return Err(UnifiedError::unprocessable(
            "Amount does not match the order total for this checkout",
        ));
    }
    Ok(())
}

/// Caller identities from the request headers. These are NOT
/// authentication — there is no global API auth layer yet — but
/// they are the only caller identity the payments module can see,
/// so mutating endpoints use them as a defence-in-depth ownership
/// check: a caller may only touch records that belong to an
/// identity they present. A caller with no identity headers
/// matches only `guest`-owned records; operator calls must send
/// `x-user-id` until the API-management layer provides a real
/// admin identity.
fn caller_identities(req: &HttpRequest) -> Vec<String> {
    let mut identities = Vec::new();
    for header in ["x-user-id", "x-session-id"] {
        if let Some(value) = req
            .headers()
            .get(header)
            .and_then(|value| value.to_str().ok())
        {
            let value = value.trim();
            if !value.is_empty() {
                identities.push(value.to_string());
            }
        }
    }
    identities
}

fn caller_owns(req: &HttpRequest, owner: &str) -> bool {
    let identities = caller_identities(req);
    if identities.is_empty() {
        return owner == "guest";
    }
    identities.iter().any(|identity| identity == owner)
}

/// Fail closed when the caller does not own the record they are
/// mutating. `what` names the record in the error message.
fn require_customer(req: &HttpRequest, owner: &str, what: &str) -> UnifiedResult<()> {
    if caller_owns(req, owner) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Caller does not own this {what}"
        )))
    }
}

/// Format minor units as a major-unit decimal string for providers
/// that take amounts as decimals (`"10.50"`, or `"1050"` for
/// zero-decimal currencies).
fn major_amount(amount_minor: i64, currency: &str) -> String {
    if is_zero_decimal(currency) {
        return amount_minor.to_string();
    }
    format!("{}.{:02}", amount_minor / 100, (amount_minor % 100).abs())
}

/// Major units as a float (Mastercard gateway amounts).
fn major_f64(amount_minor: i64, currency: &str) -> f64 {
    if is_zero_decimal(currency) {
        amount_minor as f64
    } else {
        amount_minor as f64 / 100.0
    }
}

/// Major units as an integer, for providers whose API takes amounts
/// in whole major units (Xendit charges / refunds / disbursements,
/// Midtrans gross amounts). Rounds half-up for two-decimal
/// currencies; zero-decimal currencies pass through unchanged.
fn major_i64(amount_minor: i64, currency: &str) -> i64 {
    if is_zero_decimal(currency) {
        amount_minor
    } else if amount_minor >= 0 {
        (amount_minor + 50) / 100
    } else {
        (amount_minor - 50) / 100
    }
}

/// Validate an amount and currency before any provider call.
fn validate_money(amount_minor: i64, currency: &str) -> UnifiedResult<String> {
    if amount_minor <= 0 {
        return Err(UnifiedError::bad_request(
            "Amount must be greater than zero (integer minor units)",
        ));
    }
    if amount_minor > MAX_AMOUNT_MINOR {
        return Err(UnifiedError::bad_request(
            "Amount exceeds the maximum accepted for one charge",
        ));
    }
    validate_currency(currency)
}

/// Validate an ISO 4217 currency code (three uppercase letters).
fn validate_currency(currency: &str) -> UnifiedResult<String> {
    let code = currency.trim().to_uppercase();
    if code.len() != 3 || !code.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(format!(
            "Currency \"{currency}\" is not a three-letter ISO 4217 code"
        )));
    }
    Ok(code)
}

/// Reject a value that looks like a raw card number (13-19 digits,
/// ignoring spaces and dashes). Tokens are opaque strings; a PAN is
/// never a valid token.
fn looks_like_pan(value: &str) -> bool {
    let digits: String = value.chars().filter(|c| c.is_ascii_digit()).collect();
    let stripped: String = value
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == ' ' || *c == '-')
        .collect();
    stripped.len() == value.len() && (13..=19).contains(&digits.len()) && digits.len() == value.replace([' ', '-'], "").len()
}

/// Validate a payment method token: present, non-empty, and not a PAN.
fn validate_token(token: Option<&str>, provider: &str) -> UnifiedResult<String> {
    let Some(token) = token.map(str::trim).filter(|t| !t.is_empty()) else {
        return Err(UnifiedError::bad_request(format!(
            "Provider \"{provider}\" needs a tokenized payment method (card numbers are never accepted)"
        )));
    };
    if looks_like_pan(token) {
        return Err(UnifiedError::bad_request(
            "Raw card numbers are never accepted; send a token from a PCI-compliant tokenization step",
        ));
    }
    Ok(token.to_string())
}

/// Look up the currency (and amount) of a stored transaction by its
/// provider transaction id. Capture / refund calls that only receive
/// an amount need the original currency; guessing it would charge the
/// wrong currency, so a missing record is an error at the call site.
fn stored_transaction(provider_transaction_id: &str) -> Option<PaymentTransaction> {
    transactions_store()
        .iter()
        .find(|entry| entry.value().provider_transaction_id == provider_transaction_id)
        .map(|entry| entry.value().clone())
}

fn stored_transaction_currency(provider_transaction_id: &str) -> UnifiedResult<String> {
    stored_transaction(provider_transaction_id)
        .map(|transaction| transaction.currency)
        .ok_or_else(|| {
            UnifiedError::not_found(format!(
                "No stored transaction for provider transaction id \"{provider_transaction_id}\"; capture needs the original currency"
            ))
        })
}

/// Map a provider-native status string onto the shared status.
fn map_status(kind: PaymentProviderKind, raw: &str) -> PaymentStatus {
    let raw = raw.to_ascii_lowercase();
    match kind {
        PaymentProviderKind::Stripe => match raw.as_str() {
            "succeeded" => PaymentStatus::Captured,
            "requires_capture" => PaymentStatus::Authorized,
            "requires_action" | "requires_payment_method" | "processing" => {
                PaymentStatus::Pending
            }
            "canceled" => PaymentStatus::Cancelled,
            _ => PaymentStatus::Failed,
        },
        PaymentProviderKind::PayPal => match raw.as_str() {
            "completed" | "captured" => PaymentStatus::Captured,
            "created" | "authorized" => PaymentStatus::Authorized,
            "pending" | "approval_pending" => PaymentStatus::Pending,
            "voided" => PaymentStatus::Voided,
            "refunded" => PaymentStatus::Refunded,
            "denied" => PaymentStatus::Failed,
            _ => PaymentStatus::Failed,
        },
        PaymentProviderKind::Xendit => match raw.as_str() {
            "succeeded" | "paid" | "completed" => PaymentStatus::Captured,
            "authorized" => PaymentStatus::Authorized,
            "pending" => PaymentStatus::Pending,
            "failed" | "expired" => PaymentStatus::Failed,
            _ => PaymentStatus::Failed,
        },
        PaymentProviderKind::Midtrans => match raw.as_str() {
            "capture" | "settlement" => PaymentStatus::Captured,
            "authorize" => PaymentStatus::Authorized,
            "pending" => PaymentStatus::Pending,
            "deny" | "cancel" | "expire" => PaymentStatus::Failed,
            "refund" => PaymentStatus::Refunded,
            _ => PaymentStatus::Failed,
        },
        _ => match raw.as_str() {
            "succeeded" | "success" | "captured" | "completed" | "paid" | "approved"
            | "complete" => PaymentStatus::Captured,
            "authorized" | "authorised" | "authreceived" => PaymentStatus::Authorized,
            "pending" | "processing" | "requires_action" | "approval_pending"
            | "in_progress" => PaymentStatus::Pending,
            "refunded" => PaymentStatus::Refunded,
            "voided" | "void" | "cancelled" | "canceled" => PaymentStatus::Voided,
            "chargeback" => PaymentStatus::Chargeback,
            _ => PaymentStatus::Failed,
        },
    }
}

// ---------------------------------------------------------------------------
// The dispatching adapter: one struct, one typed client per provider.
// ---------------------------------------------------------------------------

/// The built-in adapter. Each provider kind is served by its own
/// private `*_charge` / `*_refund` / ... function below, so the
/// per-provider request shapes stay typed and reviewable while the
/// registry, capability guards, and stores stay shared.
pub struct ProviderAdapter {
    kind: PaymentProviderKind,
    mode: GatewayMode,
    connection_id: Uuid,
    endpoint: Option<String>,
    credentials: PaymentCredentials,
    custom_config: Option<CustomGatewayConfig>,
    delegate_connection_id: Option<Uuid>,
    client: reqwest::Client,
}

impl ProviderAdapter {
    pub fn new(connection: &PaymentConnection) -> UnifiedResult<Self> {
        if connection.kind.needs_endpoint() && connection.endpoint.is_none() {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" needs an endpoint on the connection",
                connection.kind.as_str()
            )));
        }
        if let Some(endpoint) = &connection.endpoint {
            match connection.kind {
                PaymentProviderKind::Mastercard => validate_mastercard_endpoint(endpoint)?,
                PaymentProviderKind::Card | PaymentProviderKind::Custom => {
                    validate_payment_endpoint(Some(endpoint), connection.kind.as_str())?;
                }
                _ => {}
            }
        }
        Ok(Self {
            kind: connection.kind,
            mode: connection.mode,
            connection_id: connection.id,
            endpoint: connection.endpoint.clone(),
            credentials: connection.credentials.clone(),
            custom_config: connection.custom_config.clone(),
            delegate_connection_id: connection.delegate_connection_id,
            client: build_client()?,
        })
    }

    /// Base URL for the provider in the connection's mode.
    fn base(&self) -> UnifiedResult<String> {
        let base = match self.kind {
            PaymentProviderKind::Stripe => "https://api.stripe.com".to_string(),
            PaymentProviderKind::PayPal => {
                if self.mode == GatewayMode::Sandbox {
                    "https://api-m.sandbox.paypal.com".to_string()
                } else {
                    "https://api-m.paypal.com".to_string()
                }
            }
            PaymentProviderKind::Xendit => "https://api.xendit.co".to_string(),
            PaymentProviderKind::Midtrans => {
                if self.mode == GatewayMode::Sandbox {
                    "https://api.sandbox.midtrans.com".to_string()
                } else {
                    "https://api.midtrans.com".to_string()
                }
            }
            PaymentProviderKind::HyperPay => {
                if self.mode == GatewayMode::Sandbox {
                    "https://eu-test.oppwa.com".to_string()
                } else {
                    "https://eu-prod.oppwa.com".to_string()
                }
            }
            PaymentProviderKind::TwoCheckout => "https://api.2checkout.com".to_string(),
            PaymentProviderKind::Mastercard
            | PaymentProviderKind::Card
            | PaymentProviderKind::Custom => self.endpoint.clone().ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Provider \"{}\" needs an endpoint on the connection",
                    self.kind.as_str()
                ))
            })?,
            PaymentProviderKind::GooglePay | PaymentProviderKind::ApplePay => {
                return Err(UnifiedError::bad_request(
                    "Wallet providers have no direct API base URL; charges run through the delegate connection",
                ));
            }
        };
        Ok(base.trim_end_matches('/').to_string())
    }

    /// Join a path onto the base URL and enforce the host allow-list.
    fn url(&self, path: &str) -> UnifiedResult<Url> {
        let base = self.base()?;
        let url = Url::parse(&format!("{base}{path}")).map_err(|_| {
            UnifiedError::internal(format!(
                "Cannot build {} request URL for path \"{path}\"",
                self.kind.as_str()
            ))
        })?;
        check_allowlist(&url, self.kind)?;
        Ok(url)
    }

    /// Synchronous auth headers for providers that use a static
    /// credential. PayPal uses `paypal_bearer` instead (OAuth2 token
    /// exchange), and Custom uses `custom_headers`.
    fn auth_headers(&self) -> UnifiedResult<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let kind = self.kind.as_str();
        match self.kind {
            PaymentProviderKind::Stripe => {
                let key = self
                    .credentials
                    .secret_key
                    .as_ref()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| UnifiedError::bad_request("Stripe connection needs secret_key"))?;
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| {
                        UnifiedError::internal("Stored Stripe credential is not a valid header value")
                    })?,
                );
            }
            PaymentProviderKind::Xendit => {
                let key = self
                    .credentials
                    .api_key
                    .as_ref()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| UnifiedError::bad_request("Xendit connection needs api_key"))?;
                let basic = base64::engine::general_purpose::STANDARD.encode(format!("{key}:"));
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Basic {basic}")).map_err(|_| {
                        UnifiedError::internal("Stored Xendit credential is not a valid header value")
                    })?,
                );
            }
            PaymentProviderKind::Midtrans => {
                let key = self
                    .credentials
                    .server_key
                    .as_ref()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| UnifiedError::bad_request("Midtrans connection needs server_key"))?;
                let basic = base64::engine::general_purpose::STANDARD.encode(format!("{key}:"));
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Basic {basic}")).map_err(|_| {
                        UnifiedError::internal("Stored Midtrans credential is not a valid header value")
                    })?,
                );
            }
            PaymentProviderKind::HyperPay => {
                let token = self
                    .credentials
                    .access_token
                    .as_ref()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| {
                        UnifiedError::bad_request("HyperPay connection needs access_token")
                    })?;
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
                        UnifiedError::internal("Stored HyperPay credential is not a valid header value")
                    })?,
                );
            }
            PaymentProviderKind::TwoCheckout => {
                let merchant = self
                    .credentials
                    .merchant_id
                    .as_ref()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| {
                        UnifiedError::bad_request("2Checkout connection needs merchant_id")
                    })?;
                let secret = self
                    .credentials
                    .secret_word
                    .as_ref()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| {
                        UnifiedError::bad_request("2Checkout connection needs secret_word")
                    })?;
                // 2Checkout REST authentication: the merchant code and
                // the current date are signed with the secret word
                // (HMAC-SHA256). The date format is the one 2CO
                // documents for this header (`YYYY-MM-DD HH:MM:SS`).
                let date = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                let hash = hmac_sha256_hex(secret.as_bytes(), format!("{merchant}{date}").as_bytes());
                headers.insert(
                    "X-Avangate-Authentication",
                    HeaderValue::from_str(&format!(
                        "code=\"{merchant}\" date=\"{date}\" hash=\"{hash}\""
                    ))
                    .map_err(|_| {
                        UnifiedError::internal("Stored 2Checkout credential is not a valid header value")
                    })?,
                );
            }
            PaymentProviderKind::Mastercard => {
                let merchant = self
                    .credentials
                    .merchant_id
                    .as_ref()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| {
                        UnifiedError::bad_request("Mastercard connection needs merchant_id")
                    })?;
                let password = self
                    .credentials
                    .secret_key
                    .as_ref()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| {
                        UnifiedError::bad_request("Mastercard connection needs secret_key")
                    })?;
                let basic = base64::engine::general_purpose::STANDARD
                    .encode(format!("merchant.{merchant}:{password}"));
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Basic {basic}")).map_err(|_| {
                        UnifiedError::internal("Stored Mastercard credential is not a valid header value")
                    })?,
                );
            }
            PaymentProviderKind::Card => {
                let key = self
                    .credentials
                    .api_key
                    .as_ref()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| UnifiedError::bad_request("Card connection needs api_key"))?;
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| {
                        UnifiedError::internal("Stored card processor credential is not a valid header value")
                    })?,
                );
            }
            PaymentProviderKind::Custom => {
                return self.custom_headers();
            }
            PaymentProviderKind::PayPal | PaymentProviderKind::GooglePay | PaymentProviderKind::ApplePay => {
                return Err(UnifiedError::internal(format!(
                    "Provider \"{kind}\" does not use static auth headers"
                )));
            }
        }
        Ok(headers)
    }

    /// Auth headers for a custom gateway, per its configured auth type.
    fn custom_headers(&self) -> UnifiedResult<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let config = self.custom_config.as_ref().ok_or_else(|| {
            UnifiedError::bad_request("Custom provider connection needs custom_config")
        })?;
        let creds = &self.credentials;
        let header_value = |value: &str| {
            HeaderValue::from_str(value).map_err(|_| {
                UnifiedError::internal("Stored custom credential is not a valid header value")
            })
        };
        match config.auth_type.as_str() {
            "none" => {}
            "bearer" => {
                let key = creds.api_key.as_ref().filter(|v| !v.is_empty()).ok_or_else(|| {
                    UnifiedError::bad_request("Custom provider (bearer) needs api_key")
                })?;
                headers.insert(AUTHORIZATION, header_value(&format!("Bearer {key}"))?);
            }
            "basic" => {
                let username = creds.username.clone().unwrap_or_default();
                let password = creds.password.clone().unwrap_or_default();
                let basic = base64::engine::general_purpose::STANDARD
                    .encode(format!("{username}:{password}"));
                headers.insert(AUTHORIZATION, header_value(&format!("Basic {basic}"))?);
            }
            "api_key_header" => {
                let key = creds.api_key.as_ref().filter(|v| !v.is_empty()).ok_or_else(|| {
                    UnifiedError::bad_request("Custom provider (api_key_header) needs api_key")
                })?;
                headers.insert("X-Api-Key", header_value(key)?);
            }
            "api_key_query" => {
                // The key goes on the query string; see `custom_url`.
            }
            "hmac_sha256" => {
                let key = creds.api_key.as_ref().filter(|v| !v.is_empty()).ok_or_else(|| {
                    UnifiedError::bad_request("Custom provider (hmac_sha256) needs api_key")
                })?;
                headers.insert("X-Api-Key", header_value(key)?);
            }
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown custom auth type \"{other}\""
                )));
            }
        }
        Ok(headers)
    }

    /// A custom-gateway URL, adding the API key to the query string
    /// when the auth type is `api_key_query`.
    fn custom_url(&self, path: &str) -> UnifiedResult<Url> {
        let mut url = self.url(path)?;
        if self
            .custom_config
            .as_ref()
            .map(|config| config.auth_type.as_str())
            == Some("api_key_query")
        {
            let key = self
                .credentials
                .api_key
                .clone()
                .unwrap_or_default();
            url.query_pairs_mut().append_pair("api_key", &key);
        }
        Ok(url)
    }

    /// PayPal OAuth2 access token (client-credentials grant), cached
    /// in memory until shortly before expiry.
    async fn paypal_bearer(&self) -> UnifiedResult<String> {
        if let Some((token, expires_at)) = paypal_tokens_store().get(&self.connection_id) {
            if *expires_at > Utc::now() + chrono::Duration::seconds(60) {
                return Ok(token.clone());
            }
        }
        let client_id = self
            .credentials
            .client_id
            .clone()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| UnifiedError::bad_request("PayPal connection needs client_id"))?;
        let client_secret = self
            .credentials
            .client_secret
            .clone()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| UnifiedError::bad_request("PayPal connection needs client_secret"))?;
        let url = self.url("/v1/oauth2/token")?;
        let basic = base64::engine::general_purpose::STANDARD
            .encode(format!("{client_id}:{client_secret}"));
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .header(AUTHORIZATION, format!("Basic {basic}"))
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body("grant_type=client_credentials"),
            "paypal",
        )
        .await?;
        let token = body
            .get("access_token")
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                UnifiedError::internal("PayPal token exchange returned no access_token")
            })?
            .to_string();
        let expires_in = body
            .get("expires_in")
            .and_then(|value| value.as_i64())
            .unwrap_or(3600);
        paypal_tokens_store().insert(
            self.connection_id,
            (
                token.clone(),
                Utc::now() + chrono::Duration::seconds(expires_in),
            ),
        );
        Ok(token)
    }

    /// The delegate adapter for a wallet provider.
    fn delegate(&self) -> UnifiedResult<Arc<dyn PaymentProvider>> {
        let delegate_id = self.delegate_connection_id.ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Provider \"{}\" needs delegate_connection_id",
                self.kind.as_str()
            ))
        })?;
        let delegate = connections_store()
            .get(&delegate_id)
            .map(|entry| entry.clone())
            .ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Delegate connection \"{delegate_id}\" does not exist"
                ))
            })?;
        if !delegate.enabled {
            return Err(UnifiedError::bad_request(
                "Delegate connection is disabled",
            ));
        }
        payment_registry().build(&delegate)
    }

    /// Resolve the delegate's currency list for a wallet charge, and
    /// reject a currency the delegate cannot charge in.
    fn wallet_currency(&self, currency: &str) -> UnifiedResult<String> {
        let code = validate_currency(currency)?;
        let delegate_id = self.delegate_connection_id.ok_or_else(|| {
            UnifiedError::bad_request("Wallet provider needs delegate_connection_id")
        })?;
        let Some(delegate) = connections_store().get(&delegate_id) else {
            return Err(UnifiedError::bad_request(
                "Delegate connection does not exist",
            ));
        };
        if !currencies_for(delegate.kind).contains(&code.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Currency \"{code}\" is not supported by the delegate gateway \"{}\"",
                delegate.kind.as_str()
            )));
        }
        Ok(code)
    }
}

// ---------------------------------------------------------------------------
// `PaymentProvider` implementation: capability guards + dispatch.
// ---------------------------------------------------------------------------

#[async_trait]
impl PaymentProvider for ProviderAdapter {
    fn kind(&self) -> PaymentProviderKind {
        self.kind
    }

    fn capabilities(&self) -> PaymentCapabilities {
        match (self.kind, &self.custom_config) {
            (PaymentProviderKind::Custom, Some(config)) => config.capabilities(),
            _ => capabilities_for(self.kind),
        }
    }

    async fn test_connection(&self) -> UnifiedResult<ConnectionTestResult> {
        let started = std::time::Instant::now();
        let detail = match self.kind {
            PaymentProviderKind::Stripe => self.stripe_test().await?,
            PaymentProviderKind::PayPal => self.paypal_test().await?,
            PaymentProviderKind::Xendit => self.xendit_test().await?,
            PaymentProviderKind::Midtrans => self.midtrans_test().await?,
            PaymentProviderKind::HyperPay => self.hyperpay_test().await?,
            PaymentProviderKind::TwoCheckout => self.two_checkout_test().await?,
            PaymentProviderKind::Mastercard => self.mastercard_test().await?,
            PaymentProviderKind::Card => self.card_test().await?,
            PaymentProviderKind::Custom => self.custom_test().await?,
            PaymentProviderKind::GooglePay | PaymentProviderKind::ApplePay => {
                self.wallet_test().await?
            }
            PaymentProviderKind::BankTransfer => self.bank_transfer_test().await?,
        };
        Ok(ConnectionTestResult {
            kind: self.kind.as_str().to_string(),
            mode: self.mode.as_str().to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            detail,
        })
    }

    async fn charge(&self, input: &ChargeInput) -> UnifiedResult<ChargeResult> {
        if !self.capabilities().supports_charge {
            return Err(unsupported(self.kind, "charge", "supports_charge"));
        }
        validate_money(input.amount_minor, &input.currency)?;
        match self.kind {
            PaymentProviderKind::Stripe => self.stripe_charge(input, false).await,
            PaymentProviderKind::PayPal => self.paypal_charge(input, false).await,
            PaymentProviderKind::Xendit => self.xendit_charge(input).await,
            PaymentProviderKind::Midtrans => self.midtrans_charge(input).await,
            PaymentProviderKind::HyperPay => self.hyperpay_charge(input, "DB").await,
            PaymentProviderKind::TwoCheckout => self.two_checkout_charge(input).await,
            PaymentProviderKind::Mastercard => self.mastercard_charge(input, "PAY").await,
            PaymentProviderKind::Card => self.card_charge(input, "charge").await,
            PaymentProviderKind::Custom => self.custom_charge(input, "charge").await,
            PaymentProviderKind::GooglePay | PaymentProviderKind::ApplePay => {
                self.wallet_charge(input).await
            }
            PaymentProviderKind::BankTransfer => self.bank_transfer_charge(input).await,
        }
    }

    async fn authorize(&self, input: &ChargeInput) -> UnifiedResult<ChargeResult> {
        if !self.capabilities().supports_authorize {
            return Err(unsupported(self.kind, "authorize", "supports_authorize"));
        }
        validate_money(input.amount_minor, &input.currency)?;
        match self.kind {
            PaymentProviderKind::Stripe => self.stripe_charge(input, true).await,
            PaymentProviderKind::PayPal => self.paypal_charge(input, true).await,
            PaymentProviderKind::Xendit => self.xendit_authorize(input).await,
            PaymentProviderKind::Midtrans => self.midtrans_authorize(input).await,
            PaymentProviderKind::HyperPay => self.hyperpay_charge(input, "PA").await,
            PaymentProviderKind::Mastercard => self.mastercard_charge(input, "AUTHORIZE").await,
            PaymentProviderKind::Card => self.card_charge(input, "authorize").await,
            PaymentProviderKind::Custom => self.custom_charge(input, "authorize").await,
            PaymentProviderKind::BankTransfer => self.bank_transfer_authorize(input).await,
            _ => Err(unsupported(self.kind, "authorize", "supports_authorize")),
        }
    }

    async fn capture(
        &self,
        provider_transaction_id: &str,
        amount_minor: Option<i64>,
    ) -> UnifiedResult<ChargeResult> {
        if !self.capabilities().supports_capture {
            return Err(unsupported(self.kind, "capture", "supports_capture"));
        }
        if provider_transaction_id.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "A provider transaction id is required to capture",
            ));
        }
        if let Some(amount) = amount_minor {
            if amount <= 0 {
                return Err(UnifiedError::bad_request(
                    "Capture amount must be greater than zero",
                ));
            }
        }
        match self.kind {
            PaymentProviderKind::Stripe => {
                self.stripe_capture(provider_transaction_id, amount_minor).await
            }
            PaymentProviderKind::PayPal => {
                self.paypal_capture_authorization(provider_transaction_id, amount_minor)
                    .await
            }
            PaymentProviderKind::Xendit => {
                self.xendit_capture(provider_transaction_id, amount_minor).await
            }
            PaymentProviderKind::Midtrans => {
                self.midtrans_capture(provider_transaction_id).await
            }
            PaymentProviderKind::HyperPay => {
                self.hyperpay_capture(provider_transaction_id, amount_minor).await
            }
            PaymentProviderKind::Mastercard => {
                self.mastercard_capture(provider_transaction_id, amount_minor).await
            }
            PaymentProviderKind::Card => {
                self.card_capture(provider_transaction_id, amount_minor).await
            }
            PaymentProviderKind::Custom => {
                self.custom_capture(provider_transaction_id, amount_minor).await
            }
            _ => Err(unsupported(self.kind, "capture", "supports_capture")),
        }
    }

    async fn refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
        if !self.capabilities().supports_refund {
            return Err(unsupported(self.kind, "refund", "supports_refund"));
        }
        if let Some(amount) = input.amount_minor {
            if amount <= 0 {
                return Err(UnifiedError::bad_request(
                    "Refund amount must be greater than zero",
                ));
            }
        }
        validate_currency(&input.currency)?;
        match self.kind {
            PaymentProviderKind::Stripe => self.stripe_refund(input).await,
            PaymentProviderKind::PayPal => self.paypal_refund(input).await,
            PaymentProviderKind::Xendit => self.xendit_refund(input).await,
            PaymentProviderKind::Midtrans => self.midtrans_refund(input).await,
            PaymentProviderKind::HyperPay => self.hyperpay_refund(input).await,
            PaymentProviderKind::TwoCheckout => self.two_checkout_refund(input).await,
            PaymentProviderKind::Mastercard => self.mastercard_refund(input).await,
            PaymentProviderKind::Card => self.card_refund(input).await,
            PaymentProviderKind::Custom => self.custom_refund(input).await,
            PaymentProviderKind::GooglePay | PaymentProviderKind::ApplePay => {
                self.wallet_refund(input).await
            }
            PaymentProviderKind::BankTransfer => self.bank_transfer_refund(input).await,
        }
    }

    async fn void(&self, provider_transaction_id: &str) -> UnifiedResult<()> {
        if !self.capabilities().supports_void {
            return Err(unsupported(self.kind, "void", "supports_void"));
        }
        if provider_transaction_id.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "A provider transaction id is required to void",
            ));
        }
        match self.kind {
            PaymentProviderKind::Stripe => {
                self.stripe_void(provider_transaction_id).await
            }
            PaymentProviderKind::PayPal => {
                self.paypal_void(provider_transaction_id).await
            }
            PaymentProviderKind::HyperPay => {
                self.hyperpay_void(provider_transaction_id).await
            }
            PaymentProviderKind::Mastercard => {
                self.mastercard_void(provider_transaction_id).await
            }
            PaymentProviderKind::Card => self.card_void(provider_transaction_id).await,
            PaymentProviderKind::Custom => self.custom_void(provider_transaction_id).await,
            PaymentProviderKind::BankTransfer => {
                self.bank_transfer_void(provider_transaction_id).await
            }
            _ => Err(unsupported(self.kind, "void", "supports_void")),
        }
    }

    async fn subscription_create(
        &self,
        input: &SubscriptionInput,
    ) -> UnifiedResult<SubscriptionResult> {
        if !self.capabilities().supports_subscriptions {
            return Err(unsupported(
                self.kind,
                "subscription_create",
                "supports_subscriptions",
            ));
        }
        validate_money(input.amount_minor, &input.currency)?;
        if !matches!(input.interval.as_str(), "day" | "week" | "month" | "year") {
            return Err(UnifiedError::bad_request(
                "Subscription interval must be day, week, month, or year",
            ));
        }
        match self.kind {
            PaymentProviderKind::Stripe => self.stripe_subscription_create(input).await,
            PaymentProviderKind::PayPal => self.paypal_subscription_create(input).await,
            PaymentProviderKind::Midtrans => self.midtrans_subscription_create(input).await,
            PaymentProviderKind::Custom => self.custom_subscription_create(input).await,
            _ => self.local_schedule_subscription_create(input).await,
        }
    }

    async fn subscription_cancel(&self, provider_subscription_id: &str) -> UnifiedResult<()> {
        if !self.capabilities().supports_subscriptions {
            return Err(unsupported(
                self.kind,
                "subscription_cancel",
                "supports_subscriptions",
            ));
        }
        if provider_subscription_id.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "A provider subscription id is required to cancel",
            ));
        }
        match self.kind {
            PaymentProviderKind::Stripe => {
                self.stripe_subscription_cancel(provider_subscription_id)
                    .await
            }
            PaymentProviderKind::PayPal => {
                self.paypal_subscription_cancel(provider_subscription_id)
                    .await
            }
            PaymentProviderKind::Midtrans => {
                self.midtrans_subscription_cancel(provider_subscription_id)
                    .await
            }
            PaymentProviderKind::Custom => {
                self.custom_subscription_cancel(provider_subscription_id)
                    .await
            }
            // Local-schedule providers have no provider-side object to
            // cancel; the subscription record is cancelled by the
            // handler, which stops future renewal charges.
            _ => Ok(()),
        }
    }

    async fn webhook_verify(
        &self,
        headers: &HeaderMap,
        raw_body: &[u8],
    ) -> UnifiedResult<bool> {
        match self.kind {
            PaymentProviderKind::Stripe => self.stripe_webhook_verify(headers, raw_body),
            PaymentProviderKind::PayPal => {
                self.paypal_webhook_verify(headers, raw_body).await
            }
            PaymentProviderKind::Xendit => self.xendit_webhook_verify(headers, raw_body),
            PaymentProviderKind::Midtrans => self.midtrans_webhook_verify(raw_body),
            PaymentProviderKind::HyperPay => {
                self.hyperpay_webhook_verify(raw_body).await
            }
            PaymentProviderKind::TwoCheckout => {
                self.two_checkout_webhook_verify(raw_body).await
            }
            PaymentProviderKind::Mastercard => {
                self.mastercard_webhook_verify(headers, raw_body)
            }
            PaymentProviderKind::Card => self.card_webhook_verify(headers, raw_body),
            PaymentProviderKind::Custom => self.custom_webhook_verify(headers, raw_body),
            PaymentProviderKind::GooglePay | PaymentProviderKind::ApplePay => Err(
                UnifiedError::bad_request(
                    "Wallet providers have no server webhook of their own; payment events arrive through the delegate gateway's webhook",
                ),
            ),
            PaymentProviderKind::BankTransfer => Err(UnifiedError::bad_request(
                "Bank transfer has no provider webhook; verification is a manual administrator step in the bank-transfer module",
            )),
        }
    }

    async fn webhook_outcome(&self, raw_body: &[u8]) -> UnifiedResult<Option<PaymentStatus>> {
        match self.kind {
            PaymentProviderKind::HyperPay => self
                .hyperpay_webhook_outcome(raw_body)
                .await
                .map(Some),
            PaymentProviderKind::TwoCheckout => self
                .two_checkout_webhook_outcome(raw_body)
                .await
                .map(Some),
            _ => Ok(None),
        }
    }

    async fn payout(&self, input: &PayoutInput) -> UnifiedResult<PayoutResult> {
        if !self.capabilities().supports_payouts {
            return Err(unsupported(self.kind, "payout", "supports_payouts"));
        }
        validate_money(input.amount_minor, &input.currency)?;
        if input.destination_account.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "A payout destination account is required",
            ));
        }
        match self.kind {
            PaymentProviderKind::Stripe => self.stripe_payout(input).await,
            PaymentProviderKind::PayPal => self.paypal_payout(input).await,
            PaymentProviderKind::Xendit => self.xendit_payout(input).await,
            PaymentProviderKind::Custom => self.custom_payout(input).await,
            _ => Err(unsupported(self.kind, "payout", "supports_payouts")),
        }
    }
}

// ---------------------------------------------------------------------------
// Stripe.
// ---------------------------------------------------------------------------

impl ProviderAdapter {
    /// Stripe charge / authorize. Stripe's API is form-encoded.
    /// `authorize` creates the PaymentIntent with manual capture.
    async fn stripe_charge(
        &self,
        input: &ChargeInput,
        authorize_only: bool,
    ) -> UnifiedResult<ChargeResult> {
        let token = validate_token(input.payment_method_token.as_deref(), "stripe")?;
        let url = self.url("/v1/payment_intents")?;
        let mut form: HashMap<String, String> = HashMap::new();
        form.insert("amount".to_string(), input.amount_minor.to_string());
        form.insert("currency".to_string(), input.currency.to_lowercase());
        form.insert("payment_method".to_string(), token);
        form.insert("confirm".to_string(), "true".to_string());
        form.insert(
            "capture_method".to_string(),
            if authorize_only { "manual" } else { "automatic" }.to_string(),
        );
        if let Some(description) = &input.description {
            form.insert("description".to_string(), description.clone());
        }
        if let Some(order_id) = &input.order_id {
            form.insert("metadata[order_id]".to_string(), order_id.clone());
        }
        if let Some(invoice_id) = &input.invoice_id {
            form.insert("metadata[invoice_id]".to_string(), invoice_id.clone());
        }
        if input.three_ds {
            form.insert("payment_method_options[card][request_three_d_secure]".to_string(), "any".to_string());
        }
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .header("Idempotency-Key", input.idempotency_key.clone())
                .form(&form),
            "stripe",
        )
        .await?;
        let id = json_str(&body, &["id"])
            .ok_or_else(|| UnifiedError::internal("Stripe response has no payment intent id"))?
            .to_string();
        let raw_status = json_str(&body, &["status"]).unwrap_or("unknown");
        let status = map_status(PaymentProviderKind::Stripe, raw_status);
        let redirect_url = body
            .get("next_action")
            .and_then(|action| action.get("redirect_to_url"))
            .and_then(|target| target.get("url"))
            .and_then(|value| value.as_str())
            .map(str::to_string);
        Ok(ChargeResult {
            provider_transaction_id: id,
            status,
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
            fee_minor: fee_from(&body),
            redirect_url: redirect_url.clone(),
            requires_action: redirect_url.is_some() || raw_status == "requires_action",
            authorization_code: None,
            message: None,
        })
    }

    async fn stripe_capture(
        &self,
        provider_transaction_id: &str,
        amount_minor: Option<i64>,
    ) -> UnifiedResult<ChargeResult> {
        let url = self.url(&format!("/v1/payment_intents/{provider_transaction_id}/capture"))?;
        let mut form: HashMap<String, String> = HashMap::new();
        if let Some(amount) = amount_minor {
            form.insert("amount_to_capture".to_string(), amount.to_string());
        }
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .form(&form),
            "stripe",
        )
        .await?;
        let amount = body
            .get("amount")
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        let currency = body
            .get("currency")
            .and_then(|value| value.as_str())
            .unwrap_or("USD")
            .to_uppercase();
        let raw_status = json_str(&body, &["status"]).unwrap_or("unknown");
        Ok(ChargeResult {
            provider_transaction_id: provider_transaction_id.to_string(),
            status: map_status(PaymentProviderKind::Stripe, raw_status),
            amount_minor: amount,
            currency,
            fee_minor: fee_from(&body),
            redirect_url: None,
            requires_action: false,
            authorization_code: None,
            message: None,
        })
    }

    async fn stripe_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
        let url = self.url("/v1/refunds")?;
        let mut form: HashMap<String, String> = HashMap::new();
        form.insert(
            "payment_intent".to_string(),
            input.provider_transaction_id.clone(),
        );
        if let Some(amount) = input.amount_minor {
            form.insert("amount".to_string(), amount.to_string());
        }
        if let Some(reason) = &input.reason {
            form.insert("reason".to_string(), reason.clone());
        }
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .header("Idempotency-Key", input.idempotency_key.clone())
                .form(&form),
            "stripe",
        )
        .await?;
        let id = json_str(&body, &["id"])
            .ok_or_else(|| UnifiedError::internal("Stripe refund response has no id"))?
            .to_string();
        let amount = body
            .get("amount")
            .and_then(|value| value.as_i64())
            .unwrap_or_else(|| input.amount_minor.unwrap_or(0));
        Ok(RefundResult {
            provider_refund_id: id,
            status: PaymentStatus::Refunded,
            amount_minor: amount,
            currency: input.currency.clone(),
        })
    }

    async fn stripe_void(&self, provider_transaction_id: &str) -> UnifiedResult<()> {
        let url = self.url(&format!("/v1/payment_intents/{provider_transaction_id}/cancel"))?;
        send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .form(&HashMap::<String, String>::new()),
            "stripe",
        )
        .await?;
        Ok(())
    }

    async fn stripe_subscription_create(
        &self,
        input: &SubscriptionInput,
    ) -> UnifiedResult<SubscriptionResult> {
        let plan_id = input.plan_id.clone().ok_or_else(|| {
            UnifiedError::bad_request("Stripe subscriptions need plan_id (the Stripe price id)")
        })?;
        let url = self.url("/v1/subscriptions")?;
        let mut form: HashMap<String, String> = HashMap::new();
        form.insert("customer".to_string(), input.customer_id.clone());
        form.insert("items[0][price]".to_string(), plan_id);
        if input.trial_days > 0 {
            form.insert(
                "trial_period_days".to_string(),
                input.trial_days.to_string(),
            );
        }
        if let Some(order_id) = &input.order_id {
            form.insert("metadata[order_id]".to_string(), order_id.clone());
        }
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .header("Idempotency-Key", input.idempotency_key.clone())
                .form(&form),
            "stripe",
        )
        .await?;
        let id = json_str(&body, &["id"])
            .ok_or_else(|| UnifiedError::internal("Stripe subscription response has no id"))?
            .to_string();
        let raw_status = json_str(&body, &["status"]).unwrap_or("active");
        let period_end = body
            .get("current_period_end")
            .and_then(|value| value.as_i64())
            .and_then(|epoch| DateTime::from_timestamp(epoch, 0));
        Ok(SubscriptionResult {
            provider_subscription_id: id,
            status: raw_status.to_string(),
            current_period_end: period_end,
        })
    }

    async fn stripe_subscription_cancel(&self, provider_subscription_id: &str) -> UnifiedResult<()> {
        let url = self.url(&format!("/v1/subscriptions/{provider_subscription_id}"))?;
        send_json(
            self.client
                .request(Method::DELETE, url)
                .headers(self.auth_headers()?),
            "stripe",
        )
        .await?;
        Ok(())
    }

    async fn stripe_payout(&self, input: &PayoutInput) -> UnifiedResult<PayoutResult> {
        let url = self.url("/v1/transfers")?;
        let mut form: HashMap<String, String> = HashMap::new();
        form.insert("amount".to_string(), input.amount_minor.to_string());
        form.insert("currency".to_string(), input.currency.to_lowercase());
        form.insert(
            "destination".to_string(),
            input.destination_account.clone(),
        );
        if let Some(description) = &input.description {
            form.insert("description".to_string(), description.clone());
        }
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .header("Idempotency-Key", input.idempotency_key.clone())
                .form(&form),
            "stripe",
        )
        .await?;
        let id = json_str(&body, &["id"])
            .ok_or_else(|| UnifiedError::internal("Stripe transfer response has no id"))?
            .to_string();
        Ok(PayoutResult {
            provider_payout_id: id,
            status: PaymentStatus::Pending,
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
        })
    }

    async fn stripe_test(&self) -> UnifiedResult<String> {
        let url = self.url("/v1/balance")?;
        let (status, _) = call_raw(
            self.client
                .request(Method::GET, url)
                .headers(self.auth_headers()?),
            "stripe",
        )
        .await?;
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(UnifiedError::unauthorized(
                "Stripe rejected the secret key (HTTP 401/403)",
            ));
        }
        if !status.is_success() {
            return Err(UnifiedError::internal(format!(
                "Stripe balance check failed with HTTP {status}"
            )));
        }
        Ok("Stripe API reachable; secret key accepted".to_string())
    }

    /// Stripe webhook verification: HMAC-SHA256 over
    /// `"{timestamp}.{payload}"` with the webhook secret, compared
    /// against the `v1` signature in the `Stripe-Signature` header.
    fn stripe_webhook_verify(
        &self,
        headers: &HeaderMap,
        raw_body: &[u8],
    ) -> UnifiedResult<bool> {
        let secret = self
            .credentials
            .webhook_secret
            .clone()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                UnifiedError::bad_request("Stripe connection needs webhook_secret to verify webhooks")
            })?;
        let signature = headers
            .get("Stripe-Signature")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| {
                UnifiedError::bad_request("Webhook is missing the Stripe-Signature header")
            })?;
        let mut timestamp: Option<&str> = None;
        let mut v1: Option<&str> = None;
        for part in signature.split(',') {
            if let Some(value) = part.strip_prefix("t=") {
                timestamp = Some(value);
            } else if let Some(value) = part.strip_prefix("v1=") {
                v1 = Some(value);
            }
        }
        let (Some(timestamp), Some(v1)) = (timestamp, v1) else {
            return Ok(false);
        };
        // Replay protection: Stripe's own tolerance is five minutes.
        // A signed payload older than that is rejected even though
        // its signature is valid.
        let timestamp_secs: i64 = match timestamp.parse() {
            Ok(value) => value,
            Err(_) => return Ok(false),
        };
        let now_secs = Utc::now().timestamp();
        if (now_secs - timestamp_secs).abs() > STRIPE_WEBHOOK_TOLERANCE_SECS {
            return Ok(false);
        }
        let signed = format!("{timestamp}.{}", String::from_utf8_lossy(raw_body));
        let expected = hmac_sha256_hex(secret.as_bytes(), signed.as_bytes());
        Ok(digests_equal(&expected, v1))
    }
}

// ---------------------------------------------------------------------------
// PayPal.
// ---------------------------------------------------------------------------

impl ProviderAdapter {
    /// PayPal charge / authorize. With a payment method token the order
    /// is created and captured (or authorized) in one call chain;
    /// without a token the order is created and the caller must send
    /// the shopper to the approval link (status pending).
    async fn paypal_charge(
        &self,
        input: &ChargeInput,
        authorize_only: bool,
    ) -> UnifiedResult<ChargeResult> {
        let token = self.paypal_bearer().await?;
        let url = self.url("/v2/checkout/orders")?;
        let mut order_body = json!({
            "intent": if authorize_only { "AUTHORIZE" } else { "CAPTURE" },
            "purchase_units": [{
                "amount": {
                    "currency_code": input.currency,
                    "value": major_amount(input.amount_minor, &input.currency),
                },
                "description": input.description.clone().unwrap_or_else(|| "Payment".to_string()),
            }],
        });
        if let Some(order_id) = &input.order_id {
            if let Some(units) = order_body
                .get_mut("purchase_units")
                .and_then(|value| value.as_array_mut())
            {
                if let Some(unit) = units.first_mut() {
                    unit["custom_id"] = Value::String(order_id.clone());
                }
            }
        }
        if let Some(method_token) = &input.payment_method_token {
            let method_token = validate_token(Some(method_token), "paypal")?;
            order_body["payment_source"] = json!({
                "token": { "id": method_token, "type": "PAYMENT_METHOD_TOKEN" },
            });
        }
        let created = send_json(
            self.client
                .request(Method::POST, url)
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header("PayPal-Request-Id", input.idempotency_key.clone())
                .json(&order_body),
            "paypal",
        )
        .await?;
        let order_id = json_str(&created, &["id"])
            .ok_or_else(|| UnifiedError::internal("PayPal order response has no id"))?
            .to_string();
        if input.payment_method_token.is_none() {
            let approve_url = created
                .get("links")
                .and_then(|links| links.as_array())
                .and_then(|links| {
                    links.iter().find(|link| {
                        link.get("rel").and_then(|rel| rel.as_str()) == Some("approve")
                    })
                })
                .and_then(|link| link.get("href"))
                .and_then(|href| href.as_str())
                .map(str::to_string);
            return Ok(ChargeResult {
                provider_transaction_id: order_id,
                status: PaymentStatus::Pending,
                amount_minor: input.amount_minor,
                currency: input.currency.clone(),
                fee_minor: None,
                redirect_url: approve_url,
                requires_action: true,
                authorization_code: None,
                message: Some(
                    "PayPal order created; the shopper must approve it at the approval link"
                        .to_string(),
                ),
            });
        }
        if authorize_only {
            let url = self.url(&format!("/v2/checkout/orders/{order_id}/authorize"))?;
            let body = send_json(
                self.client
                    .request(Method::POST, url)
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header("PayPal-Request-Id", input.idempotency_key.clone())
                    .json(&json!({})),
                "paypal",
            )
            .await?;
            let auth_id = body
                .get("purchase_units")
                .and_then(|units| units.as_array())
                .and_then(|units| units.first())
                .and_then(|unit| unit.get("payments"))
                .and_then(|payments| payments.get("authorizations"))
                .and_then(|auths| auths.as_array())
                .and_then(|auths| auths.first())
                .and_then(|auth| auth.get("id"))
                .and_then(|id| id.as_str())
                .unwrap_or(&order_id)
                .to_string();
            return Ok(ChargeResult {
                provider_transaction_id: auth_id,
                status: PaymentStatus::Authorized,
                amount_minor: input.amount_minor,
                currency: input.currency.clone(),
                fee_minor: fee_from(&body),
                redirect_url: None,
                requires_action: false,
                authorization_code: None,
                message: None,
            });
        }
        let url = self.url(&format!("/v2/checkout/orders/{order_id}/capture"))?;
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header("PayPal-Request-Id", input.idempotency_key.clone())
                .json(&json!({})),
            "paypal",
        )
        .await?;
        let capture = body
            .get("purchase_units")
            .and_then(|units| units.as_array())
            .and_then(|units| units.first())
            .and_then(|unit| unit.get("payments"))
            .and_then(|payments| payments.get("captures"))
            .and_then(|captures| captures.as_array())
            .and_then(|captures| captures.first())
            .cloned();
        let (capture_id, raw_status) = match &capture {
            Some(capture) => (
                capture
                    .get("id")
                    .and_then(|id| id.as_str())
                    .unwrap_or(&order_id)
                    .to_string(),
                capture
                    .get("status")
                    .and_then(|status| status.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
            ),
            None => (order_id.clone(), "unknown".to_string()),
        };
        Ok(ChargeResult {
            provider_transaction_id: capture_id,
            status: map_status(PaymentProviderKind::PayPal, &raw_status),
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
            fee_minor: capture.as_ref().and_then(|capture| fee_from(capture)),
            redirect_url: None,
            requires_action: false,
            authorization_code: None,
            message: None,
        })
    }

    /// Capture a PayPal authorization (the id returned by authorize).
    async fn paypal_capture_authorization(
        &self,
        provider_transaction_id: &str,
        amount_minor: Option<i64>,
    ) -> UnifiedResult<ChargeResult> {
        let currency = stored_transaction_currency(provider_transaction_id)?;
        let token = self.paypal_bearer().await?;
        let url = self.url(&format!(
            "/v2/payments/authorizations/{provider_transaction_id}/capture"
        ))?;
        let mut body_json = json!({});
        if let Some(amount) = amount_minor {
            body_json["amount"] = json!({
                "value": major_amount(amount, &currency),
                "currency_code": currency,
            });
        }
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .json(&body_json),
            "paypal",
        )
        .await?;
        let capture_id = json_str(&body, &["id"])
            .unwrap_or(provider_transaction_id)
            .to_string();
        let raw_status = json_str(&body, &["status"]).unwrap_or("unknown");
        Ok(ChargeResult {
            provider_transaction_id: capture_id,
            status: map_status(PaymentProviderKind::PayPal, raw_status),
            amount_minor: amount_minor.unwrap_or(0),
            currency,
            fee_minor: fee_from(&body),
            redirect_url: None,
            requires_action: false,
            authorization_code: None,
            message: None,
        })
    }

    async fn paypal_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
        let token = self.paypal_bearer().await?;
        let url = self.url(&format!(
            "/v2/payments/captures/{}/refund",
            input.provider_transaction_id
        ))?;
        let mut body_json = json!({});
        if let Some(amount) = input.amount_minor {
            body_json["amount"] = json!({
                "value": major_amount(amount, &input.currency),
                "currency_code": input.currency,
            });
        }
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header("PayPal-Request-Id", input.idempotency_key.clone())
                .json(&body_json),
            "paypal",
        )
        .await?;
        let id = json_str(&body, &["id"])
            .ok_or_else(|| UnifiedError::internal("PayPal refund response has no id"))?
            .to_string();
        Ok(RefundResult {
            provider_refund_id: id,
            status: PaymentStatus::Refunded,
            amount_minor: input.amount_minor.unwrap_or(0),
            currency: input.currency.clone(),
        })
    }

    async fn paypal_void(&self, provider_transaction_id: &str) -> UnifiedResult<()> {
        let token = self.paypal_bearer().await?;
        let url = self.url(&format!(
            "/v2/payments/authorizations/{provider_transaction_id}/void"
        ))?;
        send_json(
            self.client
                .request(Method::POST, url)
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .json(&json!({})),
            "paypal",
        )
        .await?;
        Ok(())
    }

    async fn paypal_subscription_create(
        &self,
        input: &SubscriptionInput,
    ) -> UnifiedResult<SubscriptionResult> {
        let plan_id = input.plan_id.clone().ok_or_else(|| {
            UnifiedError::bad_request("PayPal subscriptions need plan_id (the PayPal plan id)")
        })?;
        // PayPal subscriptions require a subscriber email address; the
        // customer id is used when it is an email, otherwise the call
        // is rejected with a clear message instead of inventing one.
        if !input.customer_id.contains('@') {
            return Err(UnifiedError::bad_request(
                "PayPal subscriptions need the customer id to be the subscriber email address",
            ));
        }
        let token = self.paypal_bearer().await?;
        let url = self.url("/v1/billing/subscriptions")?;
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header("PayPal-Request-Id", input.idempotency_key.clone())
                .json(&json!({
                    "plan_id": plan_id,
                    "subscriber": { "email_address": input.customer_id },
                    "application_context": {
                        "brand_name": "Unified",
                        "user_action": "SUBSCRIBE_NOW",
                    },
                })),
            "paypal",
        )
        .await?;
        let id = json_str(&body, &["id"])
            .ok_or_else(|| UnifiedError::internal("PayPal subscription response has no id"))?
            .to_string();
        let raw_status = json_str(&body, &["status"]).unwrap_or("APPROVAL_PENDING");
        Ok(SubscriptionResult {
            provider_subscription_id: id,
            status: raw_status.to_lowercase(),
            current_period_end: None,
        })
    }

    async fn paypal_subscription_cancel(&self, provider_subscription_id: &str) -> UnifiedResult<()> {
        let token = self.paypal_bearer().await?;
        let url = self.url(&format!(
            "/v1/billing/subscriptions/{provider_subscription_id}/cancel"
        ))?;
        send_json(
            self.client
                .request(Method::POST, url)
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .json(&json!({ "reason": "Cancelled by merchant" })),
            "paypal",
        )
        .await?;
        Ok(())
    }

    async fn paypal_payout(&self, input: &PayoutInput) -> UnifiedResult<PayoutResult> {
        let token = self.paypal_bearer().await?;
        let url = self.url("/v1/payments/payouts")?;
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header("PayPal-Request-Id", input.idempotency_key.clone())
                .json(&json!({
                    "sender_batch_header": {
                        "sender_batch_id": input.idempotency_key,
                        "email_subject": "You have received a payout",
                    },
                    "items": [{
                        "recipient_type": "EMAIL",
                        "receiver": input.destination_account,
                        "amount": {
                            "value": major_amount(input.amount_minor, &input.currency),
                            "currency": input.currency,
                        },
                    }],
                })),
            "paypal",
        )
        .await?;
        let batch_id = body
            .get("batch_header")
            .and_then(|header| header.get("payout_batch_id"))
            .and_then(|id| id.as_str())
            .ok_or_else(|| UnifiedError::internal("PayPal payout response has no batch id"))?
            .to_string();
        Ok(PayoutResult {
            provider_payout_id: batch_id,
            status: PaymentStatus::Pending,
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
        })
    }

    async fn paypal_test(&self) -> UnifiedResult<String> {
        // The token exchange itself is the connection test: a bad
        // client id / secret fails here with 401.
        let _token = self.paypal_bearer().await?;
        Ok("PayPal API reachable; client credentials accepted".to_string())
    }

    /// PayPal webhook verification: server-side, through PayPal's
    /// verify-webhook-signature endpoint, using the transmission
    /// headers and the configured webhook id.
    async fn paypal_webhook_verify(
        &self,
        headers: &HeaderMap,
        raw_body: &[u8],
    ) -> UnifiedResult<bool> {
        let webhook_id = self
            .credentials
            .webhook_id
            .clone()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                UnifiedError::bad_request("PayPal connection needs webhook_id to verify webhooks")
            })?;
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        let transmission_id = header("Paypal-Transmission-Id").ok_or_else(|| {
            UnifiedError::bad_request("PayPal webhook is missing transmission headers")
        })?;
        let transmission_time = header("Paypal-Transmission-Time").unwrap_or_default();
        let cert_url = header("Paypal-Cert-Url").unwrap_or_default();
        let auth_algo = header("Paypal-Auth-Algo").unwrap_or_default();
        let transmission_sig = header("Paypal-Transmission-Sig").unwrap_or_default();
        let event: Value = serde_json::from_slice(raw_body).map_err(|_| {
            UnifiedError::bad_request("PayPal webhook body is not valid JSON")
        })?;
        let token = self.paypal_bearer().await?;
        let url = self.url("/v1/notifications/verify-webhook-signature")?;
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .json(&json!({
                    "transmission_id": transmission_id,
                    "transmission_time": transmission_time,
                    "cert_url": cert_url,
                    "auth_algo": auth_algo,
                    "transmission_sig": transmission_sig,
                    "webhook_id": webhook_id,
                    "webhook_event": event,
                })),
            "paypal",
        )
        .await?;
        Ok(body
            .get("verification_status")
            .and_then(|status| status.as_str())
            == Some("SUCCESS"))
    }
}

// ---------------------------------------------------------------------------
// Xendit.
// ---------------------------------------------------------------------------

impl ProviderAdapter {
    /// Xendit card charge (Charges API v2, one-time payment with a
    /// tokenized card). The provider reference is the order's
    /// reference when the caller threaded one, otherwise a generated
    /// reference in the shared format (see `charge_reference`).
    async fn xendit_charge(&self, input: &ChargeInput) -> UnifiedResult<ChargeResult> {
        let token = validate_token(input.payment_method_token.as_deref(), "xendit")?;
        let reference = charge_reference(input);
        let url = self.url("/v2/charges")?;
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .header("X-Idempotency-Key", input.idempotency_key.clone())
                .json(&json!({
                    "reference_id": reference,
                    "currency": input.currency,
                    "amount": major_i64(input.amount_minor, &input.currency),
                    "checkout_method": "ONE_TIME_PAYMENT",
                    "channel_code": "ID_CREDIT_CARD",
                    "channel_properties": { "card_token": token },
                    "metadata": { "customer_id": input.customer_id },
                })),
            "xendit",
        )
        .await?;
        self.xendit_charge_result(&body, input)
    }

    /// Xendit authorize: same charge call with capture disabled, so
    /// the funds are held and captured later.
    async fn xendit_authorize(&self, input: &ChargeInput) -> UnifiedResult<ChargeResult> {
        let token = validate_token(input.payment_method_token.as_deref(), "xendit")?;
        let reference = charge_reference(input);
        let url = self.url("/v2/charges")?;
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .header("X-Idempotency-Key", input.idempotency_key.clone())
                .json(&json!({
                    "reference_id": reference,
                    "currency": input.currency,
                    "amount": major_i64(input.amount_minor, &input.currency),
                    "checkout_method": "ONE_TIME_PAYMENT",
                    "channel_code": "ID_CREDIT_CARD",
                    "channel_properties": { "card_token": token, "capture": false },
                    "metadata": { "customer_id": input.customer_id },
                })),
            "xendit",
        )
        .await?;
        self.xendit_charge_result(&body, input)
    }

    fn xendit_charge_result(
        &self,
        body: &Value,
        input: &ChargeInput,
    ) -> UnifiedResult<ChargeResult> {
        let id = json_str(&body, &["id"])
            .ok_or_else(|| UnifiedError::internal("Xendit charge response has no id"))?
            .to_string();
        let raw_status = json_str(&body, &["status"]).unwrap_or("unknown");
        let redirect_url = body
            .get("actions")
            .and_then(|actions| {
                json_str(actions, &["desktop_web_checkout_url", "mobile_web_checkout_url", "redirect_url"])
            })
            .map(str::to_string);
        Ok(ChargeResult {
            provider_transaction_id: id,
            status: map_status(PaymentProviderKind::Xendit, raw_status),
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
            fee_minor: fee_from(body),
            redirect_url: redirect_url.clone(),
            requires_action: redirect_url.is_some(),
            authorization_code: None,
            message: None,
        })
    }

    async fn xendit_capture(
        &self,
        provider_transaction_id: &str,
        amount_minor: Option<i64>,
    ) -> UnifiedResult<ChargeResult> {
        let url = self.url(&format!("/v2/charges/{provider_transaction_id}/capture"))?;
        let mut body_json = json!({});
        if let Some(amount) = amount_minor {
            body_json["amount"] = Value::from(amount);
        }
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .json(&body_json),
            "xendit",
        )
        .await?;
        let raw_status = json_str(&body, &["status"]).unwrap_or("unknown");
        let amount = body
            .get("amount")
            .and_then(|value| value.as_i64())
            .unwrap_or_else(|| amount_minor.unwrap_or(0));
        let currency = body
            .get("currency")
            .and_then(|value| value.as_str())
            .unwrap_or("IDR")
            .to_string();
        Ok(ChargeResult {
            provider_transaction_id: provider_transaction_id.to_string(),
            status: map_status(PaymentProviderKind::Xendit, raw_status),
            amount_minor: amount,
            currency,
            fee_minor: fee_from(&body),
            redirect_url: None,
            requires_action: false,
            authorization_code: None,
            message: None,
        })
    }

    async fn xendit_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
        let url = self.url("/v2/refunds")?;
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .header("X-Idempotency-Key", input.idempotency_key.clone())
                .json(&json!({
                    "charge_id": input.provider_transaction_id,
                    "amount": input.amount_minor.map(|amount| major_i64(amount, &input.currency)),
                    "reason": input.reason.clone().unwrap_or_else(|| "REQUESTED_BY_CUSTOMER".to_string()),
                })),
            "xendit",
        )
        .await?;
        let id = json_str(&body, &["id"])
            .ok_or_else(|| UnifiedError::internal("Xendit refund response has no id"))?
            .to_string();
        let amount = body
            .get("amount")
            .and_then(|value| value.as_i64())
            .unwrap_or_else(|| input.amount_minor.unwrap_or(0));
        Ok(RefundResult {
            provider_refund_id: id,
            status: PaymentStatus::Refunded,
            amount_minor: amount,
            currency: input.currency.clone(),
        })
    }

    /// Xendit payout (Disbursements API). `destination_type` is the
    /// bank code, `destination_account` the account number.
    async fn xendit_payout(&self, input: &PayoutInput) -> UnifiedResult<PayoutResult> {
        let bank_code = input.destination_type.clone().ok_or_else(|| {
            UnifiedError::bad_request(
                "Xendit payouts need destination_type (the bank code, for example BCA)",
            )
        })?;
        let url = self.url("/v2/disbursements")?;
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .header("X-Idempotency-Key", input.idempotency_key.clone())
                .json(&json!({
                    "external_id": reference_id::generate_with_prefix("PYOT"),
                    "amount": major_i64(input.amount_minor, &input.currency),
                    "bank_code": bank_code,
                    "account_holder_name": input.description.clone().unwrap_or_default(),
                    "account_number": input.destination_account,
                    "description": input.description.clone().unwrap_or_default(),
                })),
            "xendit",
        )
        .await?;
        let id = json_str(&body, &["id"])
            .ok_or_else(|| UnifiedError::internal("Xendit disbursement response has no id"))?
            .to_string();
        let raw_status = json_str(&body, &["status"]).unwrap_or("PENDING");
        Ok(PayoutResult {
            provider_payout_id: id,
            status: map_status(PaymentProviderKind::Xendit, raw_status),
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
        })
    }

    async fn xendit_test(&self) -> UnifiedResult<String> {
        let url = self.url("/v2/balance?account_type=CASH")?;
        let (status, _) = call_raw(
            self.client
                .request(Method::GET, url)
                .headers(self.auth_headers()?),
            "xendit",
        )
        .await?;
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(UnifiedError::unauthorized(
                "Xendit rejected the secret key (HTTP 401/403)",
            ));
        }
        if !status.is_success() {
            return Err(UnifiedError::internal(format!(
                "Xendit balance check failed with HTTP {status}"
            )));
        }
        Ok("Xendit API reachable; secret key accepted".to_string())
    }

    /// Xendit webhook verification: the `X-Callback-Token` header must
    /// equal the callback token configured on the connection.
    fn xendit_webhook_verify(
        &self,
        headers: &HeaderMap,
        _raw_body: &[u8],
    ) -> UnifiedResult<bool> {
        let expected = self
            .credentials
            .callback_token
            .clone()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                UnifiedError::bad_request("Xendit connection needs callback_token to verify webhooks")
            })?;
        let token = headers
            .get("X-Callback-Token")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| {
                UnifiedError::bad_request("Xendit webhook is missing the X-Callback-Token header")
            })?;
        Ok(digests_equal(&expected, token))
    }
}

// ---------------------------------------------------------------------------
// Midtrans.
// ---------------------------------------------------------------------------

impl ProviderAdapter {
    /// Midtrans charge (Core API). The order reference is the
    /// order's reference when the caller threaded one, otherwise a
    /// generated reference in the shared format (see
    /// `charge_reference`); the provider transaction id stored by
    /// this module IS that order reference, because Midtrans capture
    /// / refund / status endpoints are all addressed by order id.
    /// The Midtrans transaction id is kept in `authorization_code`.
    async fn midtrans_charge(&self, input: &ChargeInput) -> UnifiedResult<ChargeResult> {
        let token = validate_token(input.payment_method_token.as_deref(), "midtrans")?;
        let reference = charge_reference(input);
        let url = self.url("/v2/charge")?;
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .json(&json!({
                    "payment_type": "credit_card",
                    "transaction_details": {
                        "order_id": reference,
                        "gross_amount": major_i64(input.amount_minor, &input.currency),
                    },
                    "credit_card": {
                        "token_id": token,
                        "authentication": input.three_ds,
                    },
                    "item_details": [{
                        "id": reference,
                        "price": major_i64(input.amount_minor, &input.currency),
                        "quantity": 1,
                        "name": input.description.clone().unwrap_or_else(|| "Payment".to_string()),
                    }],
                })),
            "midtrans",
        )
        .await?;
        let raw_status = json_str(&body, &["transaction_status"]).unwrap_or("unknown");
        let redirect_url = json_str(&body, &["redirect_url"]).map(str::to_string);
        Ok(ChargeResult {
            provider_transaction_id: reference,
            status: map_status(PaymentProviderKind::Midtrans, raw_status),
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
            fee_minor: fee_from(&body),
            redirect_url: redirect_url.clone(),
            requires_action: redirect_url.is_some(),
            authorization_code: json_str(&body, &["transaction_id"]).map(str::to_string),
            message: json_str(&body, &["status_message"]).map(str::to_string),
        })
    }

    /// Midtrans authorize: a card charge with 3-D Secure authentication
    /// requested is returned as `authorize` by Midtrans; this adapter
    /// performs the same Core API charge and reports the authorize
    /// status, which the caller then captures with
    /// `POST /v2/{order_id}/capture`.
    async fn midtrans_authorize(&self, input: &ChargeInput) -> UnifiedResult<ChargeResult> {
        let mut input = input.clone();
        input.three_ds = true;
        self.midtrans_charge(&input).await
    }

    async fn midtrans_capture(&self, provider_transaction_id: &str) -> UnifiedResult<ChargeResult> {
        let url = self.url(&format!("/v2/{provider_transaction_id}/capture"))?;
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .json(&json!({})),
            "midtrans",
        )
        .await?;
        let raw_status = json_str(&body, &["transaction_status"]).unwrap_or("unknown");
        // Midtrans returns gross_amount as a decimal string
        // ("10000.00") or a number; either way it is major units.
        let amount = body
            .get("gross_amount")
            .and_then(|value| {
                value
                    .as_str()
                    .and_then(|text| text.parse::<f64>().ok())
                    .or_else(|| value.as_f64())
            })
            .map(|major| (major * 100.0).round() as i64)
            .unwrap_or(0);
        let currency = body
            .get("currency")
            .and_then(|value| value.as_str())
            .unwrap_or("IDR")
            .to_string();
        Ok(ChargeResult {
            provider_transaction_id: provider_transaction_id.to_string(),
            status: map_status(PaymentProviderKind::Midtrans, raw_status),
            amount_minor: amount,
            currency,
            fee_minor: fee_from(&body),
            redirect_url: None,
            requires_action: false,
            authorization_code: json_str(&body, &["transaction_id"]).map(str::to_string),
            message: None,
        })
    }

    async fn midtrans_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
        let url = self.url(&format!("/v2/{}/refund", input.provider_transaction_id))?;
        let mut body_json = json!({
            "refund_key": input.idempotency_key,
        });
        if let Some(amount) = input.amount_minor {
            body_json["amount"] =
                Value::from(major_i64(amount, &input.currency));
        }
        if let Some(reason) = &input.reason {
            body_json["reason"] = Value::String(reason.clone());
        }
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .json(&body_json),
            "midtrans",
        )
        .await?;
        let refund_id = json_str(&body, &["refund_charge_id", "transaction_id"])
            .unwrap_or(&input.provider_transaction_id)
            .to_string();
        let raw_status = json_str(&body, &["transaction_status"]).unwrap_or("refund");
        Ok(RefundResult {
            provider_refund_id: refund_id,
            status: map_status(PaymentProviderKind::Midtrans, raw_status),
            amount_minor: input.amount_minor.unwrap_or(0),
            currency: input.currency.clone(),
        })
    }

    async fn midtrans_subscription_create(
        &self,
        input: &SubscriptionInput,
    ) -> UnifiedResult<SubscriptionResult> {
        let token = validate_token(input.payment_method_token.as_deref(), "midtrans")?;
        let name = input
            .plan_id
            .clone()
            .unwrap_or_else(|| reference_id::generate_with_prefix("SUBS"));
        let url = self.url("/v1/subscriptions")?;
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .json(&json!({
                    "name": name,
                    "amount": major_i64(input.amount_minor, &input.currency),
                    "currency": input.currency,
                    "payment_type": "credit_card",
                    "token": token,
                    "schedule": {
                        "interval": input.interval_count,
                        "interval_unit": input.interval,
                        "max_interval": 0,
                    },
                })),
            "midtrans",
        )
        .await?;
        let id = json_str(&body, &["id"])
            .ok_or_else(|| UnifiedError::internal("Midtrans subscription response has no id"))?
            .to_string();
        let raw_status = json_str(&body, &["status"]).unwrap_or("active");
        Ok(SubscriptionResult {
            provider_subscription_id: id,
            status: raw_status.to_string(),
            current_period_end: None,
        })
    }

    async fn midtrans_subscription_cancel(&self, provider_subscription_id: &str) -> UnifiedResult<()> {
        let url = self.url(&format!("/v1/subscriptions/{provider_subscription_id}/disable"))?;
        send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .json(&json!({})),
            "midtrans",
        )
        .await?;
        Ok(())
    }

    /// Midtrans has no ping endpoint, so the connection test looks up
    /// a synthetic order id: 401 means bad credentials, 404 means the
    /// credentials were accepted (no such transaction exists).
    async fn midtrans_test(&self) -> UnifiedResult<String> {
        let probe = format!("connection-test-{}", Uuid::now_v7());
        let url = self.url(&format!("/v2/{probe}/status"))?;
        let (status, _) = call_raw(
            self.client
                .request(Method::GET, url)
                .headers(self.auth_headers()?),
            "midtrans",
        )
        .await?;
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(UnifiedError::unauthorized(
                "Midtrans rejected the server key (HTTP 401/403)",
            ));
        }
        if status == StatusCode::NOT_FOUND {
            return Ok(
                "Midtrans API reachable; server key accepted (probe transaction not found, as expected)"
                    .to_string(),
            );
        }
        if !status.is_success() {
            return Err(UnifiedError::internal(format!(
                "Midtrans status check failed with HTTP {status}"
            )));
        }
        Ok("Midtrans API reachable; server key accepted".to_string())
    }

    /// Midtrans webhook verification: SHA512 over
    /// `order_id + status_code + gross_amount + server_key`, compared
    /// against the `signature_key` field in the webhook body.
    fn midtrans_webhook_verify(&self, raw_body: &[u8]) -> UnifiedResult<bool> {
        let server_key = self
            .credentials
            .server_key
            .clone()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                UnifiedError::bad_request("Midtrans connection needs server_key to verify webhooks")
            })?;
        let body: Value = serde_json::from_slice(raw_body).map_err(|_| {
            UnifiedError::bad_request("Midtrans webhook body is not valid JSON")
        })?;
        let order_id = json_str(&body, &["order_id"]).unwrap_or("");
        let status_code = json_str(&body, &["status_code"]).unwrap_or("");
        let gross_amount = json_str(&body, &["gross_amount"]).unwrap_or("");
        let signature = json_str(&body, &["signature_key"]).ok_or_else(|| {
            UnifiedError::bad_request("Midtrans webhook is missing signature_key")
        })?;
        let expected = sha512_hex(
            format!("{order_id}{status_code}{gross_amount}{server_key}").as_bytes(),
        );
        Ok(digests_equal(&expected, signature))
    }
}

// ---------------------------------------------------------------------------
// HyperPay (OPPWA).
// ---------------------------------------------------------------------------

impl ProviderAdapter {
    /// HyperPay charge. The shopper completes payment in the HyperPay
    /// hosted widget using the returned checkout id, so this call
    /// creates the checkout, fetches its status once, and usually
    /// returns `pending` with `requires_action` — the completion
    /// arrives through the webhook. `payment_type` is `DB` (debit) for
    /// a charge and `PA` (pre-authorization) for authorize.
    async fn hyperpay_charge(
        &self,
        input: &ChargeInput,
        payment_type: &str,
    ) -> UnifiedResult<ChargeResult> {
        let entity_id = self
            .credentials
            .merchant_id
            .clone()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| UnifiedError::bad_request("HyperPay connection needs merchant_id (entityId)"))?;
        let url = self.url("/v1/checkouts")?;
        let mut form: HashMap<String, String> = HashMap::new();
        form.insert("entityId".to_string(), entity_id);
        form.insert(
            "amount".to_string(),
            major_amount(input.amount_minor, &input.currency),
        );
        form.insert("currency".to_string(), input.currency.clone());
        form.insert("paymentType".to_string(), payment_type.to_string());
        if let Some(order_id) = &input.order_id {
            form.insert("merchantTransactionId".to_string(), order_id.clone());
        }
        if let Some(token) = &input.payment_method_token {
            let token = validate_token(Some(token), "hyperpay")?;
            form.insert("recurringPaymentToken".to_string(), token);
        }
        let created = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .form(&form),
            "hyperpay",
        )
        .await?;
        let checkout_id = json_str(&created, &["id"])
            .ok_or_else(|| UnifiedError::internal("HyperPay checkout response has no id"))?
            .to_string();
        // One status fetch: a debit that the shopper has not completed
        // yet reports pending here and completes through the webhook.
        let status_url = self.url(&format!("/v1/checkouts/{checkout_id}/payment"))?;
        let status_body = send_json(
            self.client
                .request(Method::GET, status_url)
                .headers(self.auth_headers()?),
            "hyperpay",
        )
        .await?;
        let result_code = status_body
            .get("result")
            .and_then(|result| result.get("code"))
            .and_then(|code| code.as_str())
            .unwrap_or("");
        let status = hyperpay_status(result_code);
        let pending = status == PaymentStatus::Pending;
        Ok(ChargeResult {
            provider_transaction_id: checkout_id,
            status,
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
            fee_minor: None,
            redirect_url: None,
            requires_action: pending,
            authorization_code: json_str(&status_body, &["id"]).map(str::to_string),
            message: if pending {
                Some(
                    "HyperPay checkout created; the shopper must complete payment in the HyperPay widget using this checkout id"
                        .to_string(),
                )
            } else {
                None
            },
        })
    }

    /// HyperPay capture (CP) / refund (RF) / void (RV) against the
    /// payments endpoint, addressed by the checkout / payment id.
    async fn hyperpay_payment_action(
        &self,
        provider_transaction_id: &str,
        payment_type: &str,
        amount_minor: Option<i64>,
        currency: &str,
    ) -> UnifiedResult<Value> {
        let entity_id = self
            .credentials
            .merchant_id
            .clone()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| UnifiedError::bad_request("HyperPay connection needs merchant_id (entityId)"))?;
        let url = self.url(&format!("/v1/payments/{provider_transaction_id}"))?;
        let mut form: HashMap<String, String> = HashMap::new();
        form.insert("entityId".to_string(), entity_id);
        form.insert("paymentType".to_string(), payment_type.to_string());
        if let Some(amount) = amount_minor {
            form.insert("amount".to_string(), major_amount(amount, currency));
            form.insert("currency".to_string(), currency.to_string());
        }
        send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .form(&form),
            "hyperpay",
        )
        .await
    }

    async fn hyperpay_capture(
        &self,
        provider_transaction_id: &str,
        amount_minor: Option<i64>,
    ) -> UnifiedResult<ChargeResult> {
        let currency = stored_transaction_currency(provider_transaction_id)?;
        let body = self
            .hyperpay_payment_action(provider_transaction_id, "CP", amount_minor, &currency)
            .await?;
        let result_code = body
            .get("result")
            .and_then(|result| result.get("code"))
            .and_then(|code| code.as_str())
            .unwrap_or("");
        Ok(ChargeResult {
            provider_transaction_id: provider_transaction_id.to_string(),
            status: hyperpay_status(result_code),
            amount_minor: amount_minor.unwrap_or(0),
            currency,
            fee_minor: None,
            redirect_url: None,
            requires_action: false,
            authorization_code: json_str(&body, &["id"]).map(str::to_string),
            message: None,
        })
    }

    async fn hyperpay_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
        let body = self
            .hyperpay_payment_action(
                &input.provider_transaction_id,
                "RF",
                input.amount_minor,
                &input.currency,
            )
            .await?;
        let id = json_str(&body, &["id"])
            .unwrap_or(&input.provider_transaction_id)
            .to_string();
        Ok(RefundResult {
            provider_refund_id: id,
            status: PaymentStatus::Refunded,
            amount_minor: input.amount_minor.unwrap_or(0),
            currency: input.currency.clone(),
        })
    }

    async fn hyperpay_void(&self, provider_transaction_id: &str) -> UnifiedResult<()> {
        // No amount is sent for a void, so no currency is needed.
        self.hyperpay_payment_action(provider_transaction_id, "RV", None, "USD")
            .await?;
        Ok(())
    }

    /// HyperPay has no lightweight ping endpoint, so the connection
    /// test queries a synthetic checkout id: 401/403 means bad
    /// credentials, any other status means the API answered.
    async fn hyperpay_test(&self) -> UnifiedResult<String> {
        let probe = format!("connection-test-{}", Uuid::now_v7());
        let url = self.url(&format!("/v1/checkouts/{probe}/payment"))?;
        let (status, _) = call_raw(
            self.client
                .request(Method::GET, url)
                .headers(self.auth_headers()?),
            "hyperpay",
        )
        .await?;
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(UnifiedError::unauthorized(
                "HyperPay rejected the access token (HTTP 401/403)",
            ));
        }
        Ok(format!(
            "HyperPay API reachable; access token accepted (probe answered HTTP {status})"
        ))
    }

    /// HyperPay webhook verification: server-side. The checkout id in
    /// the webhook body is re-fetched from HyperPay; a 200 proves the
    /// event refers to a real payment on this account.
    /// Fetch a HyperPay checkout payment, so the webhook outcome can
    /// be read from the provider instead of trusting the body.
    async fn hyperpay_fetch_payment(
        &self,
        checkout_id: &str,
    ) -> UnifiedResult<(StatusCode, Value)> {
        let url = self.url(&format!("/v1/checkouts/{checkout_id}/payment"))?;
        call_raw(
            self.client
                .request(Method::GET, url)
                .headers(self.auth_headers()?),
            "hyperpay",
        )
        .await
    }

    async fn hyperpay_webhook_verify(&self, raw_body: &[u8]) -> UnifiedResult<bool> {
        let body: Value = serde_json::from_slice(raw_body).map_err(|_| {
            UnifiedError::bad_request("HyperPay webhook body is not valid JSON")
        })?;
        let checkout_id = json_str(&body, &["id", "checkoutId", "paymentId"]).ok_or_else(|| {
            UnifiedError::bad_request("HyperPay webhook body carries no checkout id to verify")
        })?;
        let (status, _) = self.hyperpay_fetch_payment(checkout_id).await?;
        Ok(status.is_success())
    }

    /// Server-side outcome of a HyperPay webhook: the result code of
    /// the fetched checkout payment, mapped the same way the charge
    /// path maps it. A fetch failure is an error (fail closed): the
    /// receiver must not apply an outcome it could not confirm.
    async fn hyperpay_webhook_outcome(&self, raw_body: &[u8]) -> UnifiedResult<PaymentStatus> {
        let body: Value = serde_json::from_slice(raw_body).map_err(|_| {
            UnifiedError::bad_request("HyperPay webhook body is not valid JSON")
        })?;
        let checkout_id = json_str(&body, &["id", "checkoutId", "paymentId"]).ok_or_else(|| {
            UnifiedError::bad_request("HyperPay webhook body carries no checkout id to verify")
        })?;
        let (status, payment) = self.hyperpay_fetch_payment(checkout_id).await?;
        if !status.is_success() {
            return Err(UnifiedError::internal(
                "HyperPay checkout lookup failed while confirming the webhook outcome",
            ));
        }
        let result_code = payment
            .get("result")
            .and_then(|result| result.get("code"))
            .and_then(|code| code.as_str())
            .unwrap_or("");
        Ok(hyperpay_status(result_code))
    }
}

/// HyperPay result codes: `000.*` is success, `000.200.*` is pending
/// (shopper action still outstanding), everything else failed.
fn hyperpay_status(result_code: &str) -> PaymentStatus {
    if result_code.starts_with("000.200") {
        PaymentStatus::Pending
    } else if result_code.starts_with("000.") {
        PaymentStatus::Captured
    } else if result_code.is_empty() {
        PaymentStatus::Pending
    } else {
        PaymentStatus::Failed
    }
}

// ---------------------------------------------------------------------------
// 2Checkout.
// ---------------------------------------------------------------------------

impl ProviderAdapter {
    /// 2Checkout charge: places an order with a client-side token
    /// (EesToken). Billing details are required by 2CO and must be
    /// supplied on the charge request; they are never invented here.
    async fn two_checkout_charge(&self, input: &ChargeInput) -> UnifiedResult<ChargeResult> {
        let token = validate_token(input.payment_method_token.as_deref(), "2checkout")?;
        let billing = input.billing.clone().ok_or_else(|| {
            UnifiedError::bad_request(
                "2Checkout orders need billing details (name, email, country) on the charge request",
            )
        })?;
        if billing.email.trim().is_empty() || billing.country.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "2Checkout orders need a billing email and country",
            ));
        }
        let url = self.url("/rest/6.0/orders")?;
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .json(&json!({
                    "Items": [{
                        "Name": input.description.clone().unwrap_or_else(|| "Payment".to_string()),
                        "Quantity": 1,
                        "Price": {
                            "Amount": major_amount(input.amount_minor, &input.currency).parse::<f64>().unwrap_or(0.0),
                            "Currency": input.currency,
                        },
                    }],
                    "BillingDetails": {
                        "FirstName": billing.name,
                        "Email": billing.email,
                        "Country": billing.country,
                        "City": billing.city.unwrap_or_default(),
                        "Address": billing.address.unwrap_or_default(),
                    },
                    "PaymentDetails": {
                        "Type": "CC",
                        "Currency": input.currency,
                        "PaymentMethod": { "EesToken": token },
                    },
                })),
            "2checkout",
        )
        .await?;
        let ref_no = body
            .get("RefNo")
            .and_then(|value| value.as_str())
            .or_else(|| body.get("OrderNumber").and_then(|value| value.as_str()))
            .ok_or_else(|| {
                UnifiedError::internal("2Checkout order response has no reference number")
            })?
            .to_string();
        let raw_status = json_str(&body, &["Status"]).unwrap_or("unknown");
        Ok(ChargeResult {
            provider_transaction_id: ref_no,
            status: map_status(PaymentProviderKind::TwoCheckout, raw_status),
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
            fee_minor: fee_from(&body),
            redirect_url: None,
            requires_action: false,
            authorization_code: None,
            message: None,
        })
    }

    async fn two_checkout_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
        let url = self.url(&format!(
            "/rest/6.0/orders/{}/refund",
            input.provider_transaction_id
        ))?;
        let mut body_json = json!({
            "comment": input.reason.clone().unwrap_or_else(|| "Refund requested".to_string()),
        });
        if let Some(amount) = input.amount_minor {
            body_json["amount"] = Value::from(major_amount(amount, &input.currency));
            body_json["currency"] = Value::String(input.currency.clone());
        }
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .json(&body_json),
            "2checkout",
        )
        .await?;
        let id = json_str(&body, &["RefNo"])
            .unwrap_or(&input.provider_transaction_id)
            .to_string();
        Ok(RefundResult {
            provider_refund_id: id,
            status: PaymentStatus::Refunded,
            amount_minor: input.amount_minor.unwrap_or(0),
            currency: input.currency.clone(),
        })
    }

    async fn two_checkout_test(&self) -> UnifiedResult<String> {
        let url = self.url("/rest/6.0/orders?limit=1")?;
        let (status, _) = call_raw(
            self.client
                .request(Method::GET, url)
                .headers(self.auth_headers()?),
            "2checkout",
        )
        .await?;
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(UnifiedError::unauthorized(
                "2Checkout rejected the credentials (HTTP 401/403)",
            ));
        }
        if !status.is_success() {
            return Err(UnifiedError::internal(format!(
                "2Checkout orders check failed with HTTP {status}"
            )));
        }
        Ok("2Checkout API reachable; credentials accepted".to_string())
    }

    /// 2Checkout webhook verification: server-side. The native INS
    /// signature is MD5-based and no MD5 implementation is available
    /// in this codebase, so a webhook is only trusted after the sale
    /// it names is re-fetched from the 2Checkout API.
    /// Fetch a 2Checkout order, so the webhook outcome can be read
    /// from the provider instead of trusting the body.
    async fn two_checkout_fetch_order(
        &self,
        ref_no: &str,
    ) -> UnifiedResult<(StatusCode, Value)> {
        let url = self.url(&format!("/rest/6.0/orders/{ref_no}"))?;
        call_raw(
            self.client
                .request(Method::GET, url)
                .headers(self.auth_headers()?),
            "2checkout",
        )
        .await
    }

    async fn two_checkout_webhook_verify(&self, raw_body: &[u8]) -> UnifiedResult<bool> {
        let body: Value = serde_json::from_slice(raw_body).map_err(|_| {
            UnifiedError::bad_request("2Checkout webhook body is not valid JSON")
        })?;
        let ref_no = json_str(&body, &["sale_id", "RefNo", "order_number", "refno"])
            .ok_or_else(|| {
                UnifiedError::bad_request("2Checkout webhook body carries no sale reference to verify")
            })?;
        let (status, _) = self.two_checkout_fetch_order(ref_no).await?;
        Ok(status.is_success())
    }

    /// Server-side outcome of a 2Checkout webhook: the status of the
    /// fetched order, mapped the same way the charge path maps order
    /// statuses. A fetch failure is an error (fail closed).
    async fn two_checkout_webhook_outcome(&self, raw_body: &[u8]) -> UnifiedResult<PaymentStatus> {
        let body: Value = serde_json::from_slice(raw_body).map_err(|_| {
            UnifiedError::bad_request("2Checkout webhook body is not valid JSON")
        })?;
        let ref_no = json_str(&body, &["sale_id", "RefNo", "order_number", "refno"])
            .ok_or_else(|| {
                UnifiedError::bad_request("2Checkout webhook body carries no sale reference to verify")
            })?;
        let (status, order) = self.two_checkout_fetch_order(ref_no).await?;
        if !status.is_success() {
            return Err(UnifiedError::internal(
                "2Checkout order lookup failed while confirming the webhook outcome",
            ));
        }
        let raw_status = json_str(&order, &["Status"]).unwrap_or("unknown");
        Ok(map_status(PaymentProviderKind::TwoCheckout, raw_status))
    }
}

// ---------------------------------------------------------------------------
// Mastercard gateway (MPGS).
// ---------------------------------------------------------------------------

impl ProviderAdapter {
    /// Mastercard gateway transaction. `operation` is one of `PAY`,
    /// `AUTHORIZE`, `CAPTURE`, `REFUND`, `VOID`. Amounts are major
    /// units as decimals. The order id is the caller-supplied order id
    /// when present, otherwise a generated one; the transaction id is
    /// generated per call and returned as the provider transaction id.
    async fn mastercard_operation(
        &self,
        operation: &str,
        input: &ChargeInput,
        target_transaction_id: Option<&str>,
    ) -> UnifiedResult<ChargeResult> {
        let merchant = self
            .credentials
            .merchant_id
            .clone()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| UnifiedError::bad_request("Mastercard connection needs merchant_id"))?;
        let order_id = charge_reference(input);
        let txn_id = reference_id::generate_with_prefix("TXNS");
        let url = self.url(&format!(
            "/api/rest/version/100/merchant/{merchant}/order/{order_id}/transaction/{txn_id}"
        ))?;
        let mut body_json = json!({
            "apiOperation": operation,
            "order": {
                "amount": major_f64(input.amount_minor, &input.currency),
                "currency": input.currency,
            },
        });
        match operation {
            "PAY" | "AUTHORIZE" => {
                let token = validate_token(input.payment_method_token.as_deref(), "mastercard")?;
                body_json["sourceOfFunds"] = json!({ "type": "CARD", "token": token });
                // 3-D Secure: when the gateway needs a challenge it
                // returns a redirect in the response (parsed into
                // `redirect_url` below). The return URL for that
                // challenge must come from the caller's checkout
                // session; this adapter never invents one.
            }
            "CAPTURE" | "REFUND" | "VOID" => {
                if let Some(target) = target_transaction_id {
                    body_json["transaction"] =
                        json!({ "targetTransactionId": target });
                }
            }
            _ => {}
        }
        let body = send_json(
            self.client
                .request(Method::PUT, url)
                .headers(self.auth_headers()?)
                .json(&body_json),
            "mastercard",
        )
        .await?;
        let raw_result = json_str(&body, &["result"]).unwrap_or("FAILURE");
        let status = match raw_result {
            "SUCCESS" => {
                if operation == "AUTHORIZE" {
                    PaymentStatus::Authorized
                } else {
                    PaymentStatus::Captured
                }
            }
            "PENDING" => PaymentStatus::Pending,
            _ => PaymentStatus::Failed,
        };
        let redirect_url = body
            .get("authentication")
            .and_then(|auth| auth.get("redirect"))
            .and_then(|redirect| redirect.get("customized"))
            .and_then(|custom| custom.get("url"))
            .and_then(|value| value.as_str())
            .map(str::to_string);
        Ok(ChargeResult {
            provider_transaction_id: txn_id,
            status,
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
            fee_minor: fee_from(&body),
            redirect_url: redirect_url.clone(),
            requires_action: redirect_url.is_some(),
            authorization_code: None,
            message: json_str(&body, &["response", "gatewayCode"])
                .map(str::to_string),
        })
    }

    async fn mastercard_charge(
        &self,
        input: &ChargeInput,
        operation: &str,
    ) -> UnifiedResult<ChargeResult> {
        self.mastercard_operation(operation, input, None).await
    }

    async fn mastercard_capture(
        &self,
        provider_transaction_id: &str,
        amount_minor: Option<i64>,
    ) -> UnifiedResult<ChargeResult> {
        // The capture amount is taken from the original charge record
        // when the caller does not pass one; the gateway needs an
        // amount and the original currency, so both come from the
        // stored transaction instead of being guessed.
        let stored = stored_transaction(provider_transaction_id).ok_or_else(|| {
            UnifiedError::not_found(format!(
                "No stored transaction for provider transaction id \"{provider_transaction_id}\"; capture needs the original amount and currency"
            ))
        })?;
        let amount = amount_minor.unwrap_or(stored.amount_minor);
        let input = ChargeInput {
            amount_minor: amount,
            currency: stored.currency.clone(),
            customer_id: String::new(),
            order_id: None,
            reference_id: None,
            invoice_id: None,
            description: None,
            payment_method_token: None,
            idempotency_key: Uuid::now_v7().to_string(),
            three_ds: false,
            billing: None,
        };
        self.mastercard_operation("CAPTURE", &input, Some(provider_transaction_id))
            .await
    }

    async fn mastercard_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
        let amount = input.amount_minor.ok_or_else(|| {
            UnifiedError::bad_request("Mastercard refunds need amount_minor")
        })?;
        let charge_input = ChargeInput {
            amount_minor: amount,
            currency: input.currency.clone(),
            customer_id: String::new(),
            order_id: None,
            reference_id: None,
            invoice_id: None,
            description: input.reason.clone(),
            payment_method_token: None,
            idempotency_key: input.idempotency_key.clone(),
            three_ds: false,
            billing: None,
        };
        let result = self
            .mastercard_operation("REFUND", &charge_input, Some(&input.provider_transaction_id))
            .await?;
        Ok(RefundResult {
            provider_refund_id: result.provider_transaction_id,
            status: PaymentStatus::Refunded,
            amount_minor: amount,
            currency: input.currency.clone(),
        })
    }

    async fn mastercard_void(&self, provider_transaction_id: &str) -> UnifiedResult<()> {
        // The void targets the original transaction; the order block
        // carries the original amount and currency when this module
        // recorded the charge, and falls back to a minimal placeholder
        // when the authorization was created outside this module.
        let stored = stored_transaction(provider_transaction_id);
        let input = ChargeInput {
            amount_minor: stored.as_ref().map(|t| t.amount_minor).unwrap_or(1),
            currency: stored.as_ref().map(|t| t.currency.clone()).unwrap_or_else(|| "USD".to_string()),
            customer_id: String::new(),
            order_id: None,
            reference_id: None,
            invoice_id: None,
            description: None,
            payment_method_token: None,
            idempotency_key: Uuid::now_v7().to_string(),
            three_ds: false,
            billing: None,
        };
        self.mastercard_operation("VOID", &input, Some(provider_transaction_id))
            .await?;
        Ok(())
    }

    /// Mastercard has no ping endpoint; the test reads the merchant
    /// resource. 401/403 means bad credentials; any other status
    /// (including 400 for a merchant lookup without an order) means
    /// the gateway answered with these credentials.
    async fn mastercard_test(&self) -> UnifiedResult<String> {
        let merchant = self
            .credentials
            .merchant_id
            .clone()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| UnifiedError::bad_request("Mastercard connection needs merchant_id"))?;
        let url = self.url(&format!(
            "/api/rest/version/100/merchant/{merchant}"
        ))?;
        let (status, _) = call_raw(
            self.client
                .request(Method::GET, url)
                .headers(self.auth_headers()?),
            "mastercard",
        )
        .await?;
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(UnifiedError::unauthorized(
                "Mastercard gateway rejected the credentials (HTTP 401/403)",
            ));
        }
        Ok(format!(
            "Mastercard gateway reachable; credentials accepted (probe answered HTTP {status})"
        ))
    }

    /// Mastercard webhook verification: HMAC-SHA256 over the raw body
    /// with the configured webhook secret, compared against the
    /// `X-Webhook-Signature` header.
    fn mastercard_webhook_verify(
        &self,
        headers: &HeaderMap,
        raw_body: &[u8],
    ) -> UnifiedResult<bool> {
        let secret = self
            .credentials
            .webhook_secret
            .clone()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                UnifiedError::bad_request("Mastercard connection needs webhook_secret to verify webhooks")
            })?;
        let signature = headers
            .get("X-Webhook-Signature")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| {
                UnifiedError::bad_request("Mastercard webhook is missing the X-Webhook-Signature header")
            })?;
        let expected = hmac_sha256_hex(secret.as_bytes(), raw_body);
        Ok(digests_equal(&expected, signature))
    }
}

// ---------------------------------------------------------------------------
// Generic card processing.
// ---------------------------------------------------------------------------

impl ProviderAdapter {
    /// Generic card processor contract (documented, so any processor
    /// can implement it):
    ///
    ///   POST {endpoint}/v1/charges        {amount_minor, currency, token, order_id, customer_id, idempotency_key}
    ///   POST {endpoint}/v1/authorizations (same body)
    ///   POST {endpoint}/v1/captures       {transaction_id, amount_minor}
    ///   POST {endpoint}/v1/refunds        {transaction_id, amount_minor?, reason?}
    ///   POST {endpoint}/v1/voids          {transaction_id}
    ///   POST {endpoint}/v1/subscriptions  {customer_id, amount_minor, currency, interval, interval_count, token}
    ///
    /// Responses are JSON with `id` (or `transaction_id`) and `status`
    /// (`succeeded` / `authorized` / `pending` / `failed` /
    /// `refunded` / `voided`).
    async fn card_charge(
        &self,
        input: &ChargeInput,
        action: &str,
    ) -> UnifiedResult<ChargeResult> {
        let token = validate_token(input.payment_method_token.as_deref(), "card")?;
        let path = match action {
            "authorize" => "/v1/authorizations",
            _ => "/v1/charges",
        };
        let url = self.url(path)?;
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .header("X-Idempotency-Key", input.idempotency_key.clone())
                .json(&json!({
                    "amount_minor": input.amount_minor,
                    "currency": input.currency,
                    "token": token,
                    "order_id": input.order_id,
                    "customer_id": input.customer_id,
                    "idempotency_key": input.idempotency_key,
                    "description": input.description,
                    "three_ds": input.three_ds,
                })),
            "card",
        )
        .await?;
        let id = json_str(&body, &["id", "transaction_id"])
            .ok_or_else(|| UnifiedError::internal("Card processor response has no transaction id"))?
            .to_string();
        let raw_status = json_str(&body, &["status"]).unwrap_or("unknown");
        let redirect_url = json_str(&body, &["redirect_url"]).map(str::to_string);
        Ok(ChargeResult {
            provider_transaction_id: id,
            status: map_status(PaymentProviderKind::Card, raw_status),
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
            fee_minor: fee_from(&body),
            redirect_url: redirect_url.clone(),
            requires_action: redirect_url.is_some(),
            authorization_code: json_str(&body, &["authorization_code"]).map(str::to_string),
            message: None,
        })
    }

    async fn card_capture(
        &self,
        provider_transaction_id: &str,
        amount_minor: Option<i64>,
    ) -> UnifiedResult<ChargeResult> {
        let url = self.url("/v1/captures")?;
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .json(&json!({
                    "transaction_id": provider_transaction_id,
                    "amount_minor": amount_minor,
                })),
            "card",
        )
        .await?;
        let raw_status = json_str(&body, &["status"]).unwrap_or("unknown");
        let currency = json_str(&body, &["currency"])
            .map(str::to_string)
            .or_else(|| {
                stored_transaction(provider_transaction_id).map(|transaction| transaction.currency)
            })
            .unwrap_or_else(|| "USD".to_string());
        Ok(ChargeResult {
            provider_transaction_id: provider_transaction_id.to_string(),
            status: map_status(PaymentProviderKind::Card, raw_status),
            amount_minor: amount_minor.unwrap_or(0),
            currency,
            fee_minor: fee_from(&body),
            redirect_url: None,
            requires_action: false,
            authorization_code: None,
            message: None,
        })
    }

    async fn card_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
        let url = self.url("/v1/refunds")?;
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .header("X-Idempotency-Key", input.idempotency_key.clone())
                .json(&json!({
                    "transaction_id": input.provider_transaction_id,
                    "amount_minor": input.amount_minor,
                    "currency": input.currency,
                    "reason": input.reason,
                })),
            "card",
        )
        .await?;
        let id = json_str(&body, &["id", "refund_id", "transaction_id"])
            .unwrap_or(&input.provider_transaction_id)
            .to_string();
        Ok(RefundResult {
            provider_refund_id: id,
            status: PaymentStatus::Refunded,
            amount_minor: input.amount_minor.unwrap_or(0),
            currency: input.currency.clone(),
        })
    }

    async fn card_void(&self, provider_transaction_id: &str) -> UnifiedResult<()> {
        let url = self.url("/v1/voids")?;
        send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .json(&json!({ "transaction_id": provider_transaction_id })),
            "card",
        )
        .await?;
        Ok(())
    }

    /// The generic processor's status endpoint is the connection test:
    /// any answer other than 401/403 proves the endpoint answers with
    /// these credentials.
    async fn card_test(&self) -> UnifiedResult<String> {
        let url = self.url("/v1/status")?;
        let (status, _) = call_raw(
            self.client
                .request(Method::GET, url)
                .headers(self.auth_headers()?),
            "card",
        )
        .await?;
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(UnifiedError::unauthorized(
                "Card processor rejected the API key (HTTP 401/403)",
            ));
        }
        Ok(format!(
            "Card processor reachable; API key accepted (status endpoint answered HTTP {status})"
        ))
    }

    /// Generic processor webhook verification: HMAC-SHA256 over the
    /// raw body with the webhook secret, `X-Signature` header.
    fn card_webhook_verify(
        &self,
        headers: &HeaderMap,
        raw_body: &[u8],
    ) -> UnifiedResult<bool> {
        let secret = self
            .credentials
            .webhook_secret
            .clone()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                UnifiedError::bad_request("Card connection needs webhook_secret to verify webhooks")
            })?;
        let signature = headers
            .get("X-Signature")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| {
                UnifiedError::bad_request("Card processor webhook is missing the X-Signature header")
            })?;
        let expected = hmac_sha256_hex(secret.as_bytes(), raw_body);
        Ok(digests_equal(&expected, signature))
    }
}

// ---------------------------------------------------------------------------
// Custom provider.
// ---------------------------------------------------------------------------

impl ProviderAdapter {
    fn custom_config(&self) -> UnifiedResult<&CustomGatewayConfig> {
        self.custom_config.as_ref().ok_or_else(|| {
            UnifiedError::bad_request("Custom provider connection needs custom_config")
        })
    }

    /// Custom charge / authorize: POST the configured path with the
    /// standard envelope. The response transaction id and status come
    /// from the configured field names.
    async fn custom_charge(
        &self,
        input: &ChargeInput,
        action: &str,
    ) -> UnifiedResult<ChargeResult> {
        let config = self.custom_config()?.clone();
        let path = match action {
            "authorize" => config
                .authorize_path
                .clone()
                .ok_or_else(|| {
                    UnifiedError::bad_request(
                        "Custom provider has no authorize_path configured",
                    )
                })?,
            _ => config.charge_path.clone(),
        };
        let token = validate_token(input.payment_method_token.as_deref(), "custom")?;
        let url = self.custom_url(&path)?;
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.custom_headers()?)
                .header("X-Idempotency-Key", input.idempotency_key.clone())
                .json(&json!({
                    "amount_minor": input.amount_minor,
                    "currency": input.currency,
                    "token": token,
                    "order_id": input.order_id,
                    "customer_id": input.customer_id,
                    "idempotency_key": input.idempotency_key,
                    "description": input.description,
                })),
            "custom",
        )
        .await?;
        let id_field = config
            .transaction_id_field
            .clone()
            .unwrap_or_else(|| "id".to_string());
        let status_field = config
            .status_field
            .clone()
            .unwrap_or_else(|| "status".to_string());
        let id = body
            .get(&id_field)
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                UnifiedError::internal(format!(
                    "Custom provider response has no \"{id_field}\" field"
                ))
            })?
            .to_string();
        let raw_status = body
            .get(&status_field)
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        let redirect_url = json_str(&body, &["redirect_url"]).map(str::to_string);
        Ok(ChargeResult {
            provider_transaction_id: id,
            status: map_status(PaymentProviderKind::Custom, raw_status),
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
            fee_minor: fee_from(&body),
            redirect_url: redirect_url.clone(),
            requires_action: redirect_url.is_some(),
            authorization_code: None,
            message: None,
        })
    }

    async fn custom_capture(
        &self,
        provider_transaction_id: &str,
        amount_minor: Option<i64>,
    ) -> UnifiedResult<ChargeResult> {
        let config = self.custom_config()?.clone();
        let path = config.capture_path.clone().ok_or_else(|| {
            UnifiedError::bad_request("Custom provider has no capture_path configured")
        })?;
        let url = self.custom_url(&path)?;
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.custom_headers()?)
                .json(&json!({
                    "transaction_id": provider_transaction_id,
                    "amount_minor": amount_minor,
                })),
            "custom",
        )
        .await?;
        let status_field = config
            .status_field
            .clone()
            .unwrap_or_else(|| "status".to_string());
        let raw_status = body
            .get(&status_field)
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        let currency = json_str(&body, &["currency"])
            .map(str::to_string)
            .or_else(|| {
                stored_transaction(provider_transaction_id).map(|transaction| transaction.currency)
            })
            .unwrap_or_else(|| "USD".to_string());
        Ok(ChargeResult {
            provider_transaction_id: provider_transaction_id.to_string(),
            status: map_status(PaymentProviderKind::Custom, raw_status),
            amount_minor: amount_minor.unwrap_or(0),
            currency,
            fee_minor: fee_from(&body),
            redirect_url: None,
            requires_action: false,
            authorization_code: None,
            message: None,
        })
    }

    async fn custom_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
        let config = self.custom_config()?.clone();
        let path = config.refund_path.clone().ok_or_else(|| {
            UnifiedError::bad_request("Custom provider has no refund_path configured")
        })?;
        let url = self.custom_url(&path)?;
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.custom_headers()?)
                .header("X-Idempotency-Key", input.idempotency_key.clone())
                .json(&json!({
                    "transaction_id": input.provider_transaction_id,
                    "amount_minor": input.amount_minor,
                    "currency": input.currency,
                    "reason": input.reason,
                })),
            "custom",
        )
        .await?;
        let id_field = config
            .transaction_id_field
            .clone()
            .unwrap_or_else(|| "id".to_string());
        let id = body
            .get(&id_field)
            .and_then(|value| value.as_str())
            .unwrap_or(&input.provider_transaction_id)
            .to_string();
        Ok(RefundResult {
            provider_refund_id: id,
            status: PaymentStatus::Refunded,
            amount_minor: input.amount_minor.unwrap_or(0),
            currency: input.currency.clone(),
        })
    }

    async fn custom_void(&self, provider_transaction_id: &str) -> UnifiedResult<()> {
        let config = self.custom_config()?.clone();
        let path = config.void_path.clone().ok_or_else(|| {
            UnifiedError::bad_request("Custom provider has no void_path configured")
        })?;
        let url = self.custom_url(&path)?;
        send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.custom_headers()?)
                .json(&json!({ "transaction_id": provider_transaction_id })),
            "custom",
        )
        .await?;
        Ok(())
    }

    async fn custom_subscription_create(
        &self,
        input: &SubscriptionInput,
    ) -> UnifiedResult<SubscriptionResult> {
        let config = self.custom_config()?.clone();
        let path = config.subscription_path.clone().ok_or_else(|| {
            UnifiedError::bad_request("Custom provider has no subscription_path configured")
        })?;
        let token = validate_token(input.payment_method_token.as_deref(), "custom")?;
        let url = self.custom_url(&path)?;
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.custom_headers()?)
                .header("X-Idempotency-Key", input.idempotency_key.clone())
                .json(&json!({
                    "customer_id": input.customer_id,
                    "plan_id": input.plan_id,
                    "amount_minor": input.amount_minor,
                    "currency": input.currency,
                    "interval": input.interval,
                    "interval_count": input.interval_count,
                    "trial_days": input.trial_days,
                    "token": token,
                })),
            "custom",
        )
        .await?;
        let id_field = config
            .transaction_id_field
            .clone()
            .unwrap_or_else(|| "id".to_string());
        let id = body
            .get(&id_field)
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                UnifiedError::internal(format!(
                    "Custom provider subscription response has no \"{id_field}\" field"
                ))
            })?
            .to_string();
        let status_field = config
            .status_field
            .clone()
            .unwrap_or_else(|| "status".to_string());
        let raw_status = body
            .get(&status_field)
            .and_then(|value| value.as_str())
            .unwrap_or("active");
        Ok(SubscriptionResult {
            provider_subscription_id: id,
            status: raw_status.to_string(),
            current_period_end: None,
        })
    }

    async fn custom_subscription_cancel(&self, provider_subscription_id: &str) -> UnifiedResult<()> {
        let config = self.custom_config()?.clone();
        let path = config.subscription_path.clone().ok_or_else(|| {
            UnifiedError::bad_request("Custom provider has no subscription_path configured")
        })?;
        let url = self.custom_url(&format!("{path}/{provider_subscription_id}"))?;
        send_json(
            self.client
                .request(Method::DELETE, url)
                .headers(self.custom_headers()?),
            "custom",
        )
        .await?;
        Ok(())
    }

    async fn custom_payout(&self, input: &PayoutInput) -> UnifiedResult<PayoutResult> {
        let config = self.custom_config()?.clone();
        let path = config.payout_path.clone().ok_or_else(|| {
            UnifiedError::bad_request("Custom provider has no payout_path configured")
        })?;
        let url = self.custom_url(&path)?;
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.custom_headers()?)
                .header("X-Idempotency-Key", input.idempotency_key.clone())
                .json(&json!({
                    "amount_minor": input.amount_minor,
                    "currency": input.currency,
                    "destination_account": input.destination_account,
                    "destination_type": input.destination_type,
                    "description": input.description,
                })),
            "custom",
        )
        .await?;
        let id_field = config
            .transaction_id_field
            .clone()
            .unwrap_or_else(|| "id".to_string());
        let id = body
            .get(&id_field)
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                UnifiedError::internal(format!(
                    "Custom provider payout response has no \"{id_field}\" field"
                ))
            })?
            .to_string();
        Ok(PayoutResult {
            provider_payout_id: id,
            status: PaymentStatus::Pending,
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
        })
    }

    /// Custom connection test: GET the configured test path. A custom
    /// provider without a test path cannot be tested and fails here
    /// with a clear message instead of guessing an endpoint.
    async fn custom_test(&self) -> UnifiedResult<String> {
        let config = self.custom_config()?.clone();
        let path = config.test_path.clone().ok_or_else(|| {
            UnifiedError::bad_request(
                "Custom provider needs test_path configured to run a connection test",
            )
        })?;
        let url = self.custom_url(&path)?;
        let (status, _) = call_raw(
            self.client
                .request(Method::GET, url)
                .headers(self.custom_headers()?),
            "custom",
        )
        .await?;
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(UnifiedError::unauthorized(
                "Custom provider rejected the credentials (HTTP 401/403)",
            ));
        }
        if !status.is_success() {
            return Err(UnifiedError::internal(format!(
                "Custom provider test endpoint failed with HTTP {status}"
            )));
        }
        Ok("Custom provider reachable; credentials accepted".to_string())
    }

    /// Custom webhook verification, per the configured auth type:
    /// `hmac_sha256` compares HMAC-SHA256 over the raw body against
    /// the configured signature header; `bearer` compares the
    /// Authorization header against the API key; `api_key_header`
    /// compares the `X-Api-Key` header; `none` accepts everything and
    /// must only be used behind an IP allow-list (documented on the
    /// connection).
    fn custom_webhook_verify(
        &self,
        headers: &HeaderMap,
        raw_body: &[u8],
    ) -> UnifiedResult<bool> {
        let config = self.custom_config()?.clone();
        match config.auth_type.as_str() {
            "none" => Ok(true),
            "hmac_sha256" => {
                let secret = self
                    .credentials
                    .webhook_secret
                    .clone()
                    .filter(|v| !v.is_empty())
                    .or_else(|| self.credentials.secret_key.clone())
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| {
                        UnifiedError::bad_request(
                            "Custom provider (hmac_sha256) needs webhook_secret to verify webhooks",
                        )
                    })?;
                let header_name = config
                    .signature_header
                    .clone()
                    .unwrap_or_else(|| "X-Signature".to_string());
                let signature = headers
                    .get(header_name.as_str())
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        UnifiedError::bad_request(format!(
                            "Custom webhook is missing the {header_name} header"
                        ))
                    })?;
                let expected = hmac_sha256_hex(secret.as_bytes(), raw_body);
                Ok(digests_equal(&expected, signature))
            }
            "bearer" => {
                let key = self
                    .credentials
                    .api_key
                    .clone()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| {
                        UnifiedError::bad_request("Custom provider (bearer) needs api_key")
                    })?;
                let presented = headers
                    .get(AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("");
                Ok(digests_equal(&format!("Bearer {key}"), presented))
            }
            "api_key_header" | "api_key_query" => {
                let key = self
                    .credentials
                    .api_key
                    .clone()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| {
                        UnifiedError::bad_request("Custom provider needs api_key")
                    })?;
                let presented = headers
                    .get("X-Api-Key")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("");
                Ok(digests_equal(&key, presented))
            }
            "basic" => {
                let username = self.credentials.username.clone().unwrap_or_default();
                let password = self.credentials.password.clone().unwrap_or_default();
                let basic = base64::engine::general_purpose::STANDARD
                    .encode(format!("{username}:{password}"));
                let presented = headers
                    .get(AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("");
                Ok(digests_equal(&format!("Basic {basic}"), presented))
            }
            other => Err(UnifiedError::bad_request(format!(
                "Unknown custom auth type \"{other}\""
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// Wallet providers (Google Pay / Apple Pay) and local schedules.
// ---------------------------------------------------------------------------

impl ProviderAdapter {
    /// Wallet charge: the wallet payment token is forwarded to the
    /// delegate gateway, which performs the charge. The delegate's
    /// currency list applies (checked in `wallet_currency` by the
    /// caller path through `charge` — see `wallet_charge`).
    async fn wallet_charge(&self, input: &ChargeInput) -> UnifiedResult<ChargeResult> {
        let delegate = self.delegate()?;
        let mut input = input.clone();
        input.currency = self.wallet_currency(&input.currency)?;
        delegate.charge(&input).await
    }

    async fn wallet_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
        let delegate = self.delegate()?;
        delegate.refund(input).await
    }

    /// Wallet connection test: the delegate connection must exist,
    /// be enabled, and pass its own connection test.
    async fn wallet_test(&self) -> UnifiedResult<String> {
        let delegate = self.delegate()?;
        let result = delegate.test_connection().await?;
        Ok(format!(
            "Wallet delegates to {} ({}); delegate test: {}",
            delegate.kind().as_str(),
            result.mode,
            result.detail
        ))
    }

    // ------------------------------------------------------------------
    // Bank transfer (manual verification): no provider API is ever
    // called. A charge / authorization only records a pending manual
    // verification: the shopper transfers the money to a configured
    // bank account and uploads proof, and an administrator approves
    // it in the bank-transfer module. That approval is the capture
    // and emits `payment.captured`; this adapter deliberately has no
    // provider-side capture to call. Refund and void are manual
    // operations: the adapter records the request with an audit note
    // and the administrator moves the money through their own bank.
    // ------------------------------------------------------------------

    async fn bank_transfer_test(&self) -> UnifiedResult<String> {
        Ok("Bank transfer is a manual method: no provider API to test. Verification is done by an administrator against the configured bank accounts.".to_string())
    }

    fn bank_transfer_transaction_id() -> String {
        reference_id::generate_with_prefix("BTRF")
    }

    async fn bank_transfer_charge(&self, input: &ChargeInput) -> UnifiedResult<ChargeResult> {
        validate_money(input.amount_minor, &input.currency)?;
        Ok(ChargeResult {
            provider_transaction_id: Self::bank_transfer_transaction_id(),
            status: PaymentStatus::Pending,
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
            fee_minor: None,
            redirect_url: None,
            requires_action: true,
            authorization_code: None,
            message: Some(
                "Bank transfer selected. Transfer the amount to a configured bank account, then upload the payment proof for administrator verification.".to_string(),
            ),
        })
    }

    async fn bank_transfer_authorize(&self, input: &ChargeInput) -> UnifiedResult<ChargeResult> {
        // An authorization holds nothing at a bank: it opens the same
        // pending manual verification as a charge. The administrator
        // approval is the capture.
        self.bank_transfer_charge(input).await
    }

    async fn bank_transfer_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
        // Manual refund: no provider API moves the money. The returned
        // record is the audit trail entry for the administrator, who
        // transfers the amount back to the customer through their own
        // bank and keeps the proof with the verification record.
        // Honest caveat: the refund endpoint marks the original
        // transaction refunded and emits `payment.refunded` as soon
        // as this call succeeds, even though the money only moves
        // when the administrator sends it — the refund record stays
        // in status `pending` until then, so the administrator must
        // complete the bank transfer promptly. The integrator should
        // gate the original-transaction status change on a
        // `Refunded` status (see the coordinator report).
        let amount = input.amount_minor.ok_or_else(|| {
            UnifiedError::bad_request("A manual bank-transfer refund needs an explicit amount_minor")
        })?;
        if amount <= 0 {
            return Err(UnifiedError::bad_request(
                "Refund amount must be greater than zero",
            ));
        }
        Ok(RefundResult {
            provider_refund_id: reference_id::generate_with_prefix("RFND"),
            status: PaymentStatus::Pending,
            amount_minor: amount,
            currency: input.currency.clone(),
        })
    }

    async fn bank_transfer_void(&self, provider_transaction_id: &str) -> UnifiedResult<()> {
        // Manual void: there is no authorization held at a provider to
        // release. Cancelling the pending verification is the void;
        // the caller (bank-transfer module) records the audit entry.
        if provider_transaction_id.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "A provider transaction id is required to void",
            ));
        }
        Ok(())
    }

    /// Local-schedule subscription: for providers without a native
    /// subscription object (Xendit, HyperPay, 2Checkout, Mastercard,
    /// generic card), the subscription is a record in this module and
    /// the first period is charged immediately against the stored
    /// token. The provider subscription id is the initial charge id,
    /// so a renewal can always be traced back to a real charge.
    async fn local_schedule_subscription_create(
        &self,
        input: &SubscriptionInput,
    ) -> UnifiedResult<SubscriptionResult> {
        let token = input.payment_method_token.clone().ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Provider \"{}\" subscriptions need a tokenized payment method for the first period charge",
                self.kind.as_str()
            ))
        })?;
        let charge_input = ChargeInput {
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
            customer_id: input.customer_id.clone(),
            order_id: input.order_id.clone(),
            reference_id: None,
            invoice_id: None,
            description: Some(format!(
                "Subscription first period (plan {})",
                input.plan_id.clone().unwrap_or_default()
            )),
            payment_method_token: Some(token),
            idempotency_key: input.idempotency_key.clone(),
            three_ds: false,
            billing: None,
        };
        let charge = self.charge(&charge_input).await?;
        if charge.status != PaymentStatus::Captured && charge.status != PaymentStatus::Pending {
            return Err(UnifiedError::bad_request(format!(
                "Subscription first period charge failed with status \"{}\"",
                charge.status.as_str()
            )));
        }
        Ok(SubscriptionResult {
            provider_subscription_id: charge.provider_transaction_id,
            status: "active".to_string(),
            current_period_end: Some(period_end(input.interval.as_str(), input.interval_count, input.trial_days)),
        })
    }
}

/// End of the current billing period for a local-schedule
/// subscription, counting from now (plus any trial days).
fn period_end(interval: &str, interval_count: u32, trial_days: u32) -> DateTime<Utc> {
    let days: i64 = match interval {
        "day" => interval_count as i64,
        "week" => interval_count as i64 * 7,
        "year" => interval_count as i64 * 365,
        _ => interval_count as i64 * 30,
    };
    Utc::now() + chrono::Duration::days(days + trial_days as i64)
}

// ---------------------------------------------------------------------------
// Module state.
// ---------------------------------------------------------------------------

pub struct PaymentsState {
    hooks: RwLock<Arc<HookBus>>,
}

impl PaymentsState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }
}

// ---------------------------------------------------------------------------
// API request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
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
    /// Raw card details for validation only (see `CardDetailsInput`).
    /// When present, subscription creation runs the card validation
    /// gate before any provider call. Never stored.
    #[serde(default)]
    pub card: Option<CardDetailsInput>,
}

fn default_interval_count() -> u32 {
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

// ---------------------------------------------------------------------------
// Split bill request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
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

// ---------------------------------------------------------------------------
// Handler helpers.
// ---------------------------------------------------------------------------

/// Paginate an already-sorted, already-filtered list.
fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        None => true,
        Some(needle) => haystack
            .to_lowercase()
            .contains(&needle.to_lowercase()),
    }
}

fn parse_date_filter(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Resolve a stored connection by id.
fn find_connection(id: Uuid) -> UnifiedResult<PaymentConnection> {
    connections_store()
        .get(&id)
        .map(|entry| entry.clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Payment connection \"{id}\" not found")))
}

/// Build the adapter for a connection, refusing disabled connections.
fn adapter_for(connection: &PaymentConnection) -> UnifiedResult<Arc<dyn PaymentProvider>> {
    if !connection.enabled {
        return Err(UnifiedError::bad_request(format!(
            "Payment connection \"{}\" is disabled",
            connection.name
        )));
    }
    payment_registry().build(connection)
}

/// Idempotency key: the caller-supplied one, or a generated one.
fn idempotency_key(provided: Option<String>) -> String {
    provided
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
        .unwrap_or_else(|| Uuid::now_v7().to_string())
}

/// A previous transaction for this idempotency key on this
/// connection, if the call was already made.
fn find_by_idempotency(connection_id: Uuid, key: &str) -> Option<PaymentTransaction> {
    idempotency_store()
        .get(&(connection_id, key.to_string()))
        .and_then(|entry| transactions_store().get(entry.value()).map(|t| t.clone()))
}

/// Record a transaction and index its idempotency key.
fn record_transaction(mut transaction: PaymentTransaction) -> PaymentTransaction {
    transaction.id = Uuid::now_v7();
    transaction.created_at = Utc::now();
    idempotency_store().insert(
        (transaction.connection_id, transaction.idempotency_key.clone()),
        transaction.id,
    );
    transactions_store().insert(transaction.id, transaction.clone());
    transaction
}

fn new_transaction(
    connection_id: Uuid,
    provider: &str,
    kind: &str,
    customer_id: &str,
    order_id: Option<String>,
    invoice_id: Option<String>,
    amount_minor: i64,
    currency: &str,
    status: PaymentStatus,
    provider_transaction_id: String,
    idempotency_key: String,
) -> PaymentTransaction {
    PaymentTransaction {
        id: Uuid::now_v7(),
        intent_id: None,
        connection_id,
        provider: provider.to_string(),
        kind: kind.to_string(),
        customer_id: customer_id.to_string(),
        order_id,
        invoice_id,
        subscription_id: None,
        amount_minor,
        currency: currency.to_string(),
        fee_minor: None,
        status: status.as_str().to_string(),
        provider_transaction_id,
        provider_refund_id: None,
        original_transaction_id: None,
        idempotency_key,
        created_at: Utc::now(),
    }
}

/// Money already refunded (or reserved by a pending refund) against
/// one charge, summed over the refund records linked through
/// `original_transaction_id`. Completed refunds
/// (`refunded`/`partially_refunded`) and in-flight ones (`pending`)
/// both count here: a pending refund that later completes must not
/// be double-spent by a second refund in the meantime.
fn refunded_amount_minor(original_id: Uuid) -> i64 {
    transactions_store()
        .iter()
        .filter(|entry| {
            let transaction = entry.value();
            transaction.kind == "refund"
                && transaction.original_transaction_id == Some(original_id)
                && matches!(
                    transaction.status.as_str(),
                    "refunded" | "partially_refunded" | "pending"
                )
        })
        .map(|entry| entry.value().amount_minor)
        .sum()
}

/// Money whose refund has actually completed against one charge.
/// Drives the charge's status after a refund completes: at or over
/// the charge amount the charge is `refunded`, below it
/// `partially_refunded`.
fn completed_refunded_amount_minor(original_id: Uuid) -> i64 {
    transactions_store()
        .iter()
        .filter(|entry| {
            let transaction = entry.value();
            transaction.kind == "refund"
                && transaction.original_transaction_id == Some(original_id)
                && matches!(
                    transaction.status.as_str(),
                    "refunded" | "partially_refunded"
                )
        })
        .map(|entry| entry.value().amount_minor)
        .sum()
}

/// Resolve a payment method token: an explicit token wins, then a
/// stored payment method id, then `None`.
fn resolve_token(
    explicit_token: Option<&str>,
    method_id: Option<Uuid>,
    customer_id: &str,
) -> UnifiedResult<Option<String>> {
    if let Some(token) = explicit_token.map(str::trim).filter(|t| !t.is_empty()) {
        return Ok(Some(validate_token(Some(token), "request")?));
    }
    if let Some(method_id) = method_id {
        let method = methods_store()
            .get(&method_id)
            .map(|entry| entry.clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Payment method \"{method_id}\" not found"))
            })?;
        if method.customer_id != customer_id {
            return Err(UnifiedError::forbidden(
                "Payment method belongs to a different customer",
            ));
        }
        return Ok(Some(validate_token(Some(&method.provider_token), "stored method")?));
    }
    Ok(None)
}

/// Ask the scheduler to run a renewal for a subscription. The
/// scheduler only accepts known job kinds today, so the result is
/// reported back instead of assumed: `queued` is false until the
/// scheduler learns the `payment.subscription.renew` kind.
fn enqueue_renewal(state: &PaymentsState, subscription_id: Uuid, renew_at: DateTime<Utc>) -> bool {
    let context = state.bus().emit(
        "scheduler.enqueue",
        json!({
            "kind": "payment.subscription.renew",
            "event": "payment.subscription.renew",
            "payload": {
                "subscription_id": subscription_id.to_string(),
                "renew_at": renew_at.to_rfc3339(),
            },
        }),
    );
    context.results.iter().any(|result| {
        result
            .get("queued")
            .and_then(|queued| queued.as_bool())
            .unwrap_or(false)
    })
}

/// Spawn renewal work the way the scheduler-facing modules do: on the
/// current Tokio runtime when there is one, otherwise on a dedicated
/// thread with its own runtime (hook handlers are synchronous, and the
/// renewal charge needs async I/O).
fn spawn_renewal_worker<F>(future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(future);
    } else {
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(future);
        });
    }
}

/// Request an in-app notification through the hook bus (the
/// notifications module listens to `notification.requested`).
fn notify(state: &PaymentsState, user_id: &str, kind: &str, title: &str, body: &str) {
    state.emit(
        "notification.requested",
        json!({
            "user_id": user_id,
            "kind": kind,
            "title": title,
            "body": body,
        }),
    );
}

/// Catalog view for one provider kind.
#[derive(Debug, Clone, Serialize)]
struct ProviderCatalogView {
    kind: String,
    display_name: String,
    capabilities: PaymentCapabilities,
    currencies: Vec<String>,
    settings_fields: Vec<SettingField>,
    needs_delegate: bool,
    needs_endpoint: bool,
    has_fixed_hosts: bool,
}

fn catalog_view(kind: PaymentProviderKind) -> ProviderCatalogView {
    ProviderCatalogView {
        kind: kind.as_str().to_string(),
        display_name: kind.display_name().to_string(),
        capabilities: capabilities_for(kind),
        currencies: currencies_for(kind)
            .iter()
            .map(|currency| currency.to_string())
            .collect(),
        settings_fields: settings_fields_for(kind),
        needs_delegate: kind.needs_delegate(),
        needs_endpoint: kind.needs_endpoint(),
        has_fixed_hosts: kind.has_fixed_hosts(),
    }
}

// ---------------------------------------------------------------------------
// Handlers: catalog.
// ---------------------------------------------------------------------------

async fn list_providers(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<ProviderCatalogView>> =
        Ok(PaymentProviderKind::all().iter().map(|kind| catalog_view(*kind)).collect());
    json::respond(result, debug)
}

async fn get_provider(
    ctx: web::Data<Arc<AppContext>>,
    kind: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProviderCatalogView> = (|| {
        let kind = PaymentProviderKind::from_str(&kind).ok_or_else(|| {
            UnifiedError::not_found(format!("Unknown payment provider \"{kind}\""))
        })?;
        Ok(catalog_view(kind))
    })();
    json::respond(result, debug)
}

async fn get_provider_settings(
    ctx: web::Data<Arc<AppContext>>,
    kind: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let kind = PaymentProviderKind::from_str(&kind).ok_or_else(|| {
            UnifiedError::not_found(format!("Unknown payment provider \"{kind}\""))
        })?;
        Ok(json!({
            "kind": kind.as_str(),
            "display_name": kind.display_name(),
            "fields": settings_fields_for(kind),
            "currencies": currencies_for(kind),
            "capabilities": capabilities_for(kind),
            "needs_delegate": kind.needs_delegate(),
            "needs_endpoint": kind.needs_endpoint(),
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: connections.
// ---------------------------------------------------------------------------

async fn list_connections(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<PaymentConnectionView>> = (|| {
        let pagination = params.normalized();
        let mut views: Vec<PaymentConnectionView> = connections_store()
            .iter()
            .map(|entry| entry.value().view())
            .filter(|view| {
                matches_search(&format!("{} {} {}", view.name, view.kind, view.kind_display), &params)
                    && params
                        .filters
                        .get("kind")
                        .is_none_or(|kind| view.kind == *kind)
                    && params
                        .filters
                        .get("mode")
                        .is_none_or(|mode| view.mode == *mode)
                    && params.filters.get("enabled").is_none_or(|enabled| {
                        view.enabled == (*enabled == "true" || *enabled == "1")
                    })
            })
            .collect();
        views.sort_by(|a, b| a.name.cmp(&b.name));
        if pagination.order == "desc" {
            views.reverse();
        }
        Ok(paginate(views, &params))
    })();
    json::respond(result, debug)
}

async fn create_connection(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateConnectionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentConnectionView> = (|| {
        let input = body.into_inner();
        let kind = validate_connection(&input)?;
        let mode = match &input.mode {
            None => GatewayMode::Sandbox,
            Some(mode) => GatewayMode::from_str(mode).ok_or_else(|| {
                UnifiedError::bad_request(format!("Unknown gateway mode \"{mode}\""))
            })?,
        };
        let now = Utc::now();
        let connection = PaymentConnection {
            id: Uuid::now_v7(),
            name: input.name.trim().to_string(),
            kind,
            mode,
            endpoint: input.endpoint.map(|endpoint| endpoint.trim().to_string()),
            credentials: input.credentials,
            custom_config: input.custom_config,
            delegate_connection_id: input.delegate_connection_id,
            enabled: input.enabled.unwrap_or(true),
            last_tested_at: None,
            last_status: None,
            created_at: now,
            updated_at: now,
        };
        connections_store().insert(connection.id, connection.clone());
        Ok(connection.view())
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_connection(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentConnectionView> =
        find_connection(id.into_inner()).map(|connection| connection.view());
    json::respond(result, debug)
}

async fn update_connection(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    body: web::Json<UpdateConnectionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentConnectionView> = (|| {
        let id = id.into_inner();
        let update = body.into_inner();
        let mut connection = find_connection(id)?;
        // Re-validate the merged connection through the same rules as
        // creation, so an update can never leave a broken connection
        // behind.
        let merged = CreateConnectionRequest {
            name: update.name.clone().unwrap_or_else(|| connection.name.clone()),
            kind: connection.kind.as_str().to_string(),
            mode: update
                .mode
                .clone()
                .map(|mode| mode)
                .or_else(|| Some(connection.mode.as_str().to_string())),
            endpoint: update.endpoint.clone().or_else(|| connection.endpoint.clone()),
            credentials: {
                let mut creds = connection.credentials.clone();
                if let Some(patch) = update.credentials.clone() {
                    creds.merge(patch);
                }
                creds
            },
            custom_config: update
                .custom_config
                .clone()
                .or_else(|| connection.custom_config.clone()),
            delegate_connection_id: update
                .delegate_connection_id
                .or(connection.delegate_connection_id),
            enabled: update.enabled.or(Some(connection.enabled)),
        };
        validate_connection(&merged)?;
        if let Some(name) = update.name {
            connection.name = name.trim().to_string();
        }
        if let Some(mode) = update.mode {
            connection.mode = GatewayMode::from_str(&mode).ok_or_else(|| {
                UnifiedError::bad_request(format!("Unknown gateway mode \"{mode}\""))
            })?;
        }
        if update.endpoint.is_some() {
            connection.endpoint = update.endpoint.map(|endpoint| endpoint.trim().to_string());
        }
        if let Some(patch) = update.credentials {
            connection.credentials.merge(patch);
        }
        if update.custom_config.is_some() {
            connection.custom_config = update.custom_config;
        }
        if update.delegate_connection_id.is_some() {
            connection.delegate_connection_id = update.delegate_connection_id;
        }
        if let Some(enabled) = update.enabled {
            connection.enabled = enabled;
        }
        connection.updated_at = Utc::now();
        connections_store().insert(id, connection.clone());
        Ok(connection.view())
    })();
    json::respond(result, debug)
}

async fn delete_connection(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        let _connection = find_connection(id)?;
        // Refuse to delete a connection another connection delegates
        // to, or an active subscription charges through.
        let in_use_as_delegate = connections_store().iter().any(|entry| {
            entry.value().delegate_connection_id == Some(id)
        });
        if in_use_as_delegate {
            return Err(UnifiedError::conflict(
                "Connection is the delegate of another wallet connection; reassign the delegate first",
            ));
        }
        let in_use_by_subscription = subscriptions_store()
            .iter()
            .any(|entry| entry.value().connection_id == id && entry.value().status == "active");
        if in_use_by_subscription {
            return Err(UnifiedError::conflict(
                "Connection has active subscriptions; cancel them first",
            ));
        }
        connections_store().remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

async fn test_connection(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ConnectionTestResult> = (|| async {
        let id = id.into_inner();
        let connection = find_connection(id)?;
        let adapter = payment_registry().build(&connection)?;
        let test = adapter.test_connection().await?;
        if let Some(mut stored) = connections_store().get_mut(&id) {
            stored.last_tested_at = Some(Utc::now());
            stored.last_status = Some("ok".to_string());
            stored.updated_at = Utc::now();
        }
        Ok(test)
    })()
    .await;
    json::respond(result, debug)
}

async fn switch_mode(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    body: web::Json<SwitchModeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentConnectionView> = (|| {
        let id = id.into_inner();
        let mode = GatewayMode::from_str(&body.mode).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown gateway mode \"{}\" (expected sandbox or live)",
                body.mode
            ))
        })?;
        let mut connection = find_connection(id)?;
        if connection.mode == mode {
            return Err(UnifiedError::bad_request(format!(
                "Connection is already in {} mode",
                mode.as_str()
            )));
        }
        // The switch only changes the base URL; the operator must have
        // stored the matching mode's credentials on the connection.
        // A test right after the switch is the way to confirm them.
        connection.mode = mode;
        connection.updated_at = Utc::now();
        connection.last_tested_at = None;
        connection.last_status = Some(format!("switched to {}", mode.as_str()));
        connections_store().insert(id, connection.clone());
        Ok(connection.view())
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: intents.
// ---------------------------------------------------------------------------

async fn create_intent(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateIntentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentIntent> = (|| {
        let input = body.into_inner();
        if input.customer_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("customer_id is required"));
        }
        require_customer(&req, &input.customer_id, "customer")?;
        validate_money(input.amount_minor, &input.currency)?;
        check_pending_checkout_amount(
            input.order_id.as_deref(),
            input.amount_minor,
            &input.currency,
        )?;
        let connection = find_connection(input.connection_id)?;
        if !connection.enabled {
            return Err(UnifiedError::bad_request("Payment connection is disabled"));
        }
        // Wallet kinds charge through the delegate; the delegate's
        // currency list is checked at confirm time, so the intent
        // only checks the wallet's own list loosely here.
        if !connection.kind.needs_delegate()
            && !currencies_for(connection.kind).contains(&input.currency.as_str())
        {
            return Err(UnifiedError::bad_request(format!(
                "Currency \"{}\" is not supported by provider \"{}\"",
                input.currency,
                connection.kind.as_str()
            )));
        }
        let capture_method = input.capture_method.clone().unwrap_or_else(|| "automatic".to_string());
        if capture_method != "automatic" && capture_method != "manual" {
            return Err(UnifiedError::bad_request(
                "capture_method must be automatic or manual",
            ));
        }
        if capture_method == "manual" && !capabilities_for(connection.kind).supports_authorize {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" does not support manual capture (authorize)",
                connection.kind.as_str()
            )));
        }
        let token = resolve_token(
            input.payment_method_token.as_deref(),
            input.payment_method_id,
            &input.customer_id,
        )?;
        let now = Utc::now();
        let intent = PaymentIntent {
            id: Uuid::now_v7(),
            connection_id: connection.id,
            provider: connection.kind.as_str().to_string(),
            customer_id: input.customer_id,
            order_id: input.order_id,
            invoice_id: input.invoice_id,
            amount_minor: input.amount_minor,
            currency: input.currency,
            status: "requires_confirmation".to_string(),
            capture_method,
            provider_transaction_id: None,
            fee_minor: None,
            payment_method_token: token,
            payment_method_id: input.payment_method_id,
            idempotency_key: idempotency_key(input.idempotency_key),
            created_at: now,
            updated_at: now,
        };
        intents_store().insert(intent.id, intent.clone());
        Ok(intent)
    })();
    match result {
        Ok(intent) => json::success_response(intent, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_intent(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentIntent> = (|| {
        let intent = intents_store()
            .get(&id.into_inner())
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Payment intent not found"))?;
        require_customer(&req, &intent.customer_id, "intent")?;
        Ok(intent)
    })();
    json::respond(result, debug)
}

/// Confirm an intent: authorize (manual capture) or charge
/// (automatic capture), record the transaction, and emit the matching
/// event. A repeated confirm with the same idempotency key returns the
/// original transaction instead of charging twice.
async fn confirm_intent(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<ConfirmIntentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentTransaction> = (|| async {
        let id = id.into_inner();
        let request = body.into_inner();
        let mut intent = intents_store()
            .get(&id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Payment intent not found"))?;
        require_customer(&req, &intent.customer_id, "intent")?;
        // Idempotency first: a repeated confirm with the same key
        // returns the original transaction instead of charging twice
        // or failing on the already-confirmed status below.
        let connection = find_connection(intent.connection_id)?;
        if let Some(existing) = find_by_idempotency(
            connection.id,
            &request.idempotency_key.clone().unwrap_or_else(|| intent.idempotency_key.clone()),
        ) {
            return Ok(existing);
        }
        if intent.status != "requires_confirmation" {
            return Err(UnifiedError::conflict(format!(
                "Payment intent in status \"{}\" cannot be confirmed",
                intent.status
            )));
        }
        let token = resolve_token(
            request.payment_method_token.as_deref(),
            request.payment_method_id,
            &intent.customer_id,
        )?
        .or_else(|| intent.payment_method_token.clone());
        // Batch 18 (S1): validate the card before any provider
        // call. Raw details on the request run in `full` mode; a
        // stored method id (from the request or the intent) runs
        // in `method` mode; an opaque token alone skips the gate.
        request_card_validation(&state, &req, request.card.as_ref(), request.payment_method_id.or(intent.payment_method_id), None, &connection, &intent.customer_id, intent.order_id.as_deref(), Some(intent.id))?;
        let adapter = adapter_for(&connection)?;
        let charge_input = ChargeInput {
            amount_minor: intent.amount_minor,
            currency: intent.currency.clone(),
            customer_id: intent.customer_id.clone(),
            order_id: intent.order_id.clone(),
            reference_id: pending_checkout_reference(intent.order_id.as_deref()),
            invoice_id: intent.invoice_id.clone(),
            description: None,
            payment_method_token: token,
            idempotency_key: idempotency_key(request.idempotency_key),
            three_ds: false,
            billing: None,
        };
        let result = if intent.capture_method == "manual" {
            adapter.authorize(&charge_input).await?
        } else {
            adapter.charge(&charge_input).await?
        };
        let (kind, status, event) = match result.status {
            PaymentStatus::Authorized => ("authorize", PaymentStatus::Authorized, Some(PAYMENT_AUTHORIZED)),
            PaymentStatus::Captured => ("charge", PaymentStatus::Captured, Some(PAYMENT_CAPTURED)),
            PaymentStatus::Failed => ("charge", PaymentStatus::Failed, Some(PAYMENT_FAILED)),
            other => ("charge", other, None),
        };
        let mut transaction = new_transaction(
            connection.id,
            &intent.provider,
            kind,
            &intent.customer_id,
            intent.order_id.clone(),
            intent.invoice_id.clone(),
            intent.amount_minor,
            &intent.currency,
            status,
            result.provider_transaction_id.clone(),
            charge_input.idempotency_key.clone(),
        );
        transaction.intent_id = Some(intent.id);
        transaction.fee_minor = result.fee_minor;
        let transaction = record_transaction(transaction);
        intent.provider_transaction_id = Some(result.provider_transaction_id.clone());
        intent.fee_minor = result.fee_minor;
        intent.status = match result.status {
            PaymentStatus::Authorized => "authorized".to_string(),
            PaymentStatus::Captured => "captured".to_string(),
            PaymentStatus::Failed => "failed".to_string(),
            PaymentStatus::Pending => "requires_confirmation".to_string(),
            _ => "failed".to_string(),
        };
        intent.updated_at = Utc::now();
        intents_store().insert(intent.id, intent.clone());
        if let Some(event) = event {
            state.emit(
                event,
                json!({
                    "intent_id": intent.id,
                    "transaction_id": transaction.id,
                    "provider_transaction_id": result.provider_transaction_id,
                    "provider": intent.provider,
                    "connection_id": connection.id,
                    "customer_id": intent.customer_id,
                    "order_id": intent.order_id,
                    "invoice_id": intent.invoice_id,
                    "amount_minor": intent.amount_minor,
                    "currency": intent.currency,
                    "fee_minor": result.fee_minor,
                    "status": transaction.status,
                }),
            );
            if event == PAYMENT_FAILED {
                notify(
                    &state,
                    &intent.customer_id,
                    "payment_failed",
                    "Payment failed",
                    &format!(
                        "Your payment of {} {} could not be completed.",
                        intent.amount_minor, intent.currency
                    ),
                );
            }
        }
        if result.status == PaymentStatus::Failed {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" declined the payment",
                intent.provider
            ))
            .with_metadata("provider", Value::String(intent.provider.clone()))
            .with_metadata("transaction_id", Value::String(transaction.id.to_string())));
        }
        Ok(transaction)
    })()
    .await;
    json::respond(result, debug)
}

/// Capture a previously authorized intent.
async fn capture_intent(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<CaptureIntentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentTransaction> = (|| async {
        let id = id.into_inner();
        let request = body.into_inner();
        let mut intent = intents_store()
            .get(&id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Payment intent not found"))?;
        require_customer(&req, &intent.customer_id, "intent")?;
        if let Some(amount) = request.amount_minor {
            if amount <= 0 || amount > intent.amount_minor {
                return Err(UnifiedError::bad_request(
                    "Capture amount must be greater than zero and no more than the authorized amount",
                ));
            }
        }
        if intent.status != "authorized" {
            return Err(UnifiedError::conflict(format!(
                "Only an authorized intent can be captured (status is \"{}\")",
                intent.status
            )));
        }
        let provider_transaction_id = intent.provider_transaction_id.clone().ok_or_else(|| {
            UnifiedError::internal("Authorized intent has no provider transaction id")
        })?;
        let connection = find_connection(intent.connection_id)?;
        let adapter = adapter_for(&connection)?;
        let result = adapter
            .capture(&provider_transaction_id, request.amount_minor)
            .await?;
        // Only a successful capture moves the intent; a failed
        // capture leaves it authorized so the caller can retry.
        if result.status != PaymentStatus::Captured {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" capture failed with status \"{}\"",
                intent.provider,
                result.status.as_str()
            ))
            .with_metadata("provider", Value::String(intent.provider.clone())));
        }
        let mut transaction = new_transaction(
            connection.id,
            &intent.provider,
            "capture",
            &intent.customer_id,
            intent.order_id.clone(),
            intent.invoice_id.clone(),
            request.amount_minor.unwrap_or(intent.amount_minor),
            &intent.currency,
            result.status,
            result.provider_transaction_id.clone(),
            Uuid::now_v7().to_string(),
        );
        transaction.intent_id = Some(intent.id);
        transaction.fee_minor = result.fee_minor;
        let transaction = record_transaction(transaction);
        intent.status = "captured".to_string();
        intent.fee_minor = result.fee_minor;
        intent.updated_at = Utc::now();
        intents_store().insert(intent.id, intent.clone());
        state.emit(
            PAYMENT_CAPTURED,
            json!({
                "intent_id": intent.id,
                "transaction_id": transaction.id,
                "provider_transaction_id": result.provider_transaction_id,
                "provider": intent.provider,
                "connection_id": connection.id,
                "customer_id": intent.customer_id,
                "order_id": intent.order_id,
                "invoice_id": intent.invoice_id,
                "amount_minor": transaction.amount_minor,
                "currency": intent.currency,
                "fee_minor": result.fee_minor,
                "status": transaction.status,
            }),
        );
        Ok(transaction)
    })()
    .await;
    json::respond(result, debug)
}

/// Cancel an intent. An authorized intent is also voided at the
/// provider when the provider supports void; a void failure leaves
/// the intent authorized and returns the provider error.
async fn cancel_intent(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentIntent> = (|| async {
        let id = id.into_inner();
        let mut intent = intents_store()
            .get(&id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Payment intent not found"))?;
        require_customer(&req, &intent.customer_id, "intent")?;
        if intent.status != "requires_confirmation" && intent.status != "authorized" {
            return Err(UnifiedError::conflict(format!(
                "Payment intent in status \"{}\" cannot be cancelled",
                intent.status
            )));
        }
        if intent.status == "authorized" {
            let connection = find_connection(intent.connection_id)?;
            let adapter = adapter_for(&connection)?;
            if adapter.capabilities().supports_void {
                if let Some(provider_transaction_id) = &intent.provider_transaction_id {
                    adapter.void(provider_transaction_id).await?;
                }
            }
        }
        intent.status = "cancelled".to_string();
        intent.updated_at = Utc::now();
        intents_store().insert(intent.id, intent.clone());
        Ok(intent)
    })()
    .await;
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: charges, transactions, refunds, voids, payouts.
// ---------------------------------------------------------------------------

/// Direct charge (no intent): create the charge at the provider,
/// record it, and emit the matching event. Idempotent on
/// (connection, idempotency key).
async fn create_charge(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    body: web::Json<CreateChargeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentTransaction> = (|| async {
        let input = body.into_inner();
        if input.customer_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("customer_id is required"));
        }
        require_customer(&req, &input.customer_id, "customer")?;
        validate_money(input.amount_minor, &input.currency)?;
        check_pending_checkout_amount(
            input.order_id.as_deref(),
            input.amount_minor,
            &input.currency,
        )?;
        let connection = find_connection(input.connection_id)?;
        let key = idempotency_key(input.idempotency_key.clone());
        if let Some(existing) = find_by_idempotency(connection.id, &key) {
            return Ok(existing);
        }
        if !connection.kind.needs_delegate()
            && !currencies_for(connection.kind).contains(&input.currency.as_str())
        {
            return Err(UnifiedError::bad_request(format!(
                "Currency \"{}\" is not supported by provider \"{}\"",
                input.currency,
                connection.kind.as_str()
            )));
        }
        let token = resolve_token(
            input.payment_method_token.as_deref(),
            input.payment_method_id,
            &input.customer_id,
        )?;
        // Batch 18 (S2): validate the card before any provider
        // call. Raw details on the request run in `full` mode; a
        // stored method id runs in `method` mode; an opaque token
        // alone skips the gate.
        request_card_validation(&state, &req, input.card.as_ref(), input.payment_method_id, None, &connection, &input.customer_id, input.order_id.as_deref(), None)?;
        let adapter = adapter_for(&connection)?;
        let charge_input = ChargeInput {
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
            customer_id: input.customer_id.clone(),
            order_id: input.order_id.clone(),
            reference_id: pending_checkout_reference(input.order_id.as_deref()),
            invoice_id: input.invoice_id.clone(),
            description: input.description.clone(),
            payment_method_token: token,
            idempotency_key: key.clone(),
            three_ds: input.three_ds,
            billing: input.billing.map(|billing| BillingDetails {
                name: billing.name,
                email: billing.email,
                country: billing.country,
                city: billing.city,
                address: billing.address,
            }),
        };
        let result = adapter.charge(&charge_input).await?;
        let mut transaction = new_transaction(
            connection.id,
            connection.kind.as_str(),
            "charge",
            &input.customer_id,
            input.order_id.clone(),
            input.invoice_id.clone(),
            input.amount_minor,
            &input.currency,
            result.status,
            result.provider_transaction_id.clone(),
            key,
        );
        transaction.fee_minor = result.fee_minor;
        let transaction = record_transaction(transaction);
        let payload = json!({
            "transaction_id": transaction.id,
            "provider_transaction_id": result.provider_transaction_id,
            "provider": connection.kind.as_str(),
            "connection_id": connection.id,
            "customer_id": input.customer_id,
            "order_id": input.order_id,
            "invoice_id": input.invoice_id,
            "amount_minor": input.amount_minor,
            "currency": input.currency,
            "fee_minor": result.fee_minor,
            "status": transaction.status,
        });
        match result.status {
            PaymentStatus::Captured => state.emit(PAYMENT_CAPTURED, payload),
            PaymentStatus::Failed => {
                state.emit(PAYMENT_FAILED, payload);
                notify(
                    &state,
                    &input.customer_id,
                    "payment_failed",
                    "Payment failed",
                    &format!(
                        "Your payment of {} {} could not be completed.",
                        input.amount_minor, input.currency
                    ),
                );
            }
            _ => {}
        }
        if result.status == PaymentStatus::Failed {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" declined the payment",
                connection.kind.as_str()
            ))
            .with_metadata("provider", Value::String(connection.kind.as_str().to_string()))
            .with_metadata("transaction_id", Value::String(transaction.id.to_string())));
        }
        Ok(transaction)
    })()
    .await;
    match result {
        Ok(transaction) => json::success_response(transaction, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_transactions(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<PaymentTransaction>> = (|| {
        let from = params
            .filters
            .get("from")
            .and_then(|value| parse_date_filter(value));
        let to = params
            .filters
            .get("to")
            .and_then(|value| parse_date_filter(value));
        let mut items: Vec<PaymentTransaction> = transactions_store()
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|transaction| {
                matches_search(
                    &format!(
                        "{} {} {} {}",
                        transaction.provider_transaction_id,
                        transaction.provider,
                        transaction.customer_id,
                        transaction.order_id.clone().unwrap_or_default()
                    ),
                    &params,
                ) && params
                    .filters
                    .get("provider")
                    .is_none_or(|provider| transaction.provider == *provider)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| transaction.status == *status)
                    && params
                        .filters
                        .get("currency")
                        .is_none_or(|currency| transaction.currency == *currency)
                    && params
                        .filters
                        .get("customer")
                        .is_none_or(|customer| transaction.customer_id == *customer)
                    && params
                        .filters
                        .get("customer_id")
                        .is_none_or(|customer| transaction.customer_id == *customer)
                    && params
                        .filters
                        .get("order_id")
                        .is_none_or(|order_id| transaction.order_id.as_deref() == Some(order_id.as_str()))
                    && params
                        .filters
                        .get("invoice_id")
                        .is_none_or(|invoice_id| transaction.invoice_id.as_deref() == Some(invoice_id.as_str()))
                    && params
                        .filters
                        .get("kind")
                        .is_none_or(|kind| transaction.kind == *kind)
                    && from.is_none_or(|from| transaction.created_at >= from)
                    && to.is_none_or(|to| transaction.created_at <= to)
            })
            .collect();
        let sort = params.allowed_sort(&["created_at", "amount_minor"], "created_at");
        match sort.as_str() {
            "amount_minor" => items.sort_by_key(|transaction| transaction.amount_minor),
            _ => items.sort_by_key(|transaction| transaction.created_at),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn get_transaction(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentTransaction> = (|| {
        let transaction = transactions_store()
            .get(&id.into_inner())
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Payment transaction not found"))?;
        require_customer(&req, &transaction.customer_id, "transaction")?;
        Ok(transaction)
    })();
    json::respond(result, debug)
}

/// Refund a captured charge (or a charge waiting in
/// `refund_pending` for an earlier refund to finish).
///
/// The refund is only final when the provider says it is. A
/// `Refunded` / `PartiallyRefunded` result records the refund,
/// moves the original to `refunded` / `partially_refunded` from
/// the cumulative completed refunds, and emits `payment.refunded`
/// with an explicit `status` so the store and billing listeners
/// can gate on it. A `Pending` result (bank-transfer refunds)
/// records the refund with status `pending`, moves the original
/// to `refund_pending`, and emits NOTHING — the order and invoice
/// stay un-refunded until `complete_refund` closes it. Any other
/// result (failed, ...) records nothing and returns an error, so
/// a failed refund can never mark an order refunded.
async fn refund_transaction(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<CreateRefundRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentTransaction> = (|| async {
        let id = id.into_inner();
        let request = body.into_inner();
        let original = transactions_store()
            .get(&id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Payment transaction not found"))?;
        require_customer(&req, &original.customer_id, "transaction")?;
        if original.kind != "charge" && original.kind != "capture" {
            return Err(UnifiedError::unprocessable(format!(
                "Only a charge or capture can be refunded (transaction kind is \"{}\")",
                original.kind
            )));
        }
        if original.status != "captured"
            && original.status != "partially_refunded"
            && original.status != "refund_pending"
        {
            return Err(UnifiedError::unprocessable(format!(
                "Only a captured transaction can be refunded (status is \"{}\")",
                original.status
            )));
        }
        // Over-refund guard: a refund may not exceed what is left
        // of the charge after every earlier refund (completed or
        // still pending) linked to it. An absent amount refunds the
        // remainder, never the full original amount.
        let already_refunded = refunded_amount_minor(original.id);
        let remaining = original.amount_minor - already_refunded;
        if remaining <= 0 {
            return Err(UnifiedError::unprocessable(
                "This transaction has already been fully refunded",
            ));
        }
        let refund_amount = request.amount_minor.unwrap_or(remaining);
        if refund_amount <= 0 || refund_amount > remaining {
            return Err(UnifiedError::bad_request(
                "Refund amount must be greater than zero and no more than the remaining refundable amount",
            ));
        }
        let connection = find_connection(original.connection_id)?;
        let key = idempotency_key(request.idempotency_key.clone());
        if let Some(existing) = find_by_idempotency(connection.id, &key) {
            return Ok(existing);
        }
        let adapter = adapter_for(&connection)?;
        let refund = adapter
            .refund(&RefundInput {
                provider_transaction_id: original.provider_transaction_id.clone(),
                amount_minor: Some(refund_amount),
                currency: original.currency.clone(),
                reason: request.reason.clone(),
                idempotency_key: key.clone(),
            })
            .await?;
        let refund_status = refund.status;
        match refund_status {
            PaymentStatus::Refunded | PaymentStatus::PartiallyRefunded => {
                let mut refund_transaction = new_transaction(
                    connection.id,
                    &original.provider,
                    "refund",
                    &original.customer_id,
                    original.order_id.clone(),
                    original.invoice_id.clone(),
                    refund_amount,
                    &original.currency,
                    refund_status,
                    refund.provider_refund_id.clone(),
                    key,
                );
                refund_transaction.provider_refund_id = Some(refund.provider_refund_id.clone());
                refund_transaction.subscription_id = original.subscription_id;
                refund_transaction.original_transaction_id = Some(original.id);
                let refund_transaction = record_transaction(refund_transaction);
                let completed = completed_refunded_amount_minor(original.id);
                let fully_refunded = completed >= original.amount_minor;
                if let Some(mut stored) = transactions_store().get_mut(&id) {
                    stored.status = if fully_refunded {
                        "refunded".to_string()
                    } else {
                        "partially_refunded".to_string()
                    };
                }
                state.emit(
                    PAYMENT_REFUNDED,
                    json!({
                        "transaction_id": refund_transaction.id,
                        "original_transaction_id": original.id,
                        "provider_transaction_id": original.provider_transaction_id,
                        "provider_refund_id": refund.provider_refund_id,
                        "provider": original.provider,
                        "connection_id": connection.id,
                        "customer_id": original.customer_id,
                        "order_id": original.order_id,
                        "invoice_id": original.invoice_id,
                        "amount_minor": refund_amount,
                        "currency": original.currency,
                        "status": if fully_refunded { "refunded" } else { "partially_refunded" },
                        "fully_refunded": fully_refunded,
                        "reason": request.reason,
                    }),
                );
                Ok(refund_transaction)
            }
            PaymentStatus::Pending => {
                let mut refund_transaction = new_transaction(
                    connection.id,
                    &original.provider,
                    "refund",
                    &original.customer_id,
                    original.order_id.clone(),
                    original.invoice_id.clone(),
                    refund_amount,
                    &original.currency,
                    PaymentStatus::Pending,
                    refund.provider_refund_id.clone(),
                    key,
                );
                refund_transaction.provider_refund_id = Some(refund.provider_refund_id.clone());
                refund_transaction.subscription_id = original.subscription_id;
                refund_transaction.original_transaction_id = Some(original.id);
                let refund_transaction = record_transaction(refund_transaction);
                if let Some(mut stored) = transactions_store().get_mut(&id) {
                    stored.status = "refund_pending".to_string();
                }
                Ok(refund_transaction)
            }
            other => Err(UnifiedError::unprocessable(format!(
                "Provider \"{}\" did not complete the refund (status is \"{}\"); nothing was recorded",
                original.provider,
                other.as_str()
            ))
            .with_metadata("provider", Value::String(original.provider.clone()))),
        }
    })()
    .await;
    match result {
        Ok(transaction) => json::success_response(transaction, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

/// Complete a pending refund: the refund-completion path.
///
/// Takes the REFUND transaction id (kind `refund`, status
/// `pending`). `status` defaults to `refunded` and accepts
/// `refunded` or `failed`. On `refunded` the refund is marked
/// completed, the original charge's status is recomputed from the
/// cumulative completed refunds, and `payment.refunded` is emitted
/// with an explicit `status` so the store and billing listeners
/// move the order and invoice. On `failed` the refund is marked
/// failed and an original waiting in `refund_pending` returns to
/// `captured` (or `partially_refunded` when other refunds already
/// completed), with no refund event emitted. Refunds recorded by
/// the current refund path always carry their original link, so a
/// completed refund without one emits nothing.
async fn complete_refund(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<CompleteRefundRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentTransaction> = (|| {
        let id = id.into_inner();
        let request = body.into_inner();
        let outcome = request
            .status
            .clone()
            .unwrap_or_else(|| "refunded".to_string());
        if outcome != "refunded" && outcome != "failed" {
            return Err(UnifiedError::bad_request(
                "status must be \"refunded\" or \"failed\"",
            ));
        }
        let refund_transaction = transactions_store()
            .get(&id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Refund transaction not found"))?;
        require_customer(&req, &refund_transaction.customer_id, "refund")?;
        if refund_transaction.kind != "refund" {
            return Err(UnifiedError::unprocessable(format!(
                "Only a refund can be completed (transaction kind is \"{}\")",
                refund_transaction.kind
            )));
        }
        if refund_transaction.status != "pending" {
            return Err(UnifiedError::conflict(format!(
                "Only a pending refund can be completed (status is \"{}\")",
                refund_transaction.status
            )));
        }
        let original_id = refund_transaction.original_transaction_id;
        if outcome == "refunded" {
            if let Some(mut stored) = transactions_store().get_mut(&id) {
                stored.status = "refunded".to_string();
            }
            if let Some(original_id) = original_id {
                let original = transactions_store()
                    .get(&original_id)
                    .map(|entry| entry.clone());
                if let Some(original) = original {
                    let completed = completed_refunded_amount_minor(original_id);
                    let fully_refunded = completed >= original.amount_minor;
                    let new_status = if fully_refunded {
                        "refunded"
                    } else {
                        "partially_refunded"
                    };
                    if let Some(mut stored) = transactions_store().get_mut(&original_id) {
                        stored.status = new_status.to_string();
                    }
                    state.emit(
                        PAYMENT_REFUNDED,
                        json!({
                            "transaction_id": refund_transaction.id,
                            "original_transaction_id": original.id,
                            "provider_transaction_id": original.provider_transaction_id,
                            "provider_refund_id": refund_transaction.provider_refund_id,
                            "provider": refund_transaction.provider,
                            "connection_id": refund_transaction.connection_id,
                            "customer_id": refund_transaction.customer_id,
                            "order_id": refund_transaction.order_id,
                            "invoice_id": refund_transaction.invoice_id,
                            "amount_minor": refund_transaction.amount_minor,
                            "currency": refund_transaction.currency,
                            "status": if fully_refunded { "refunded" } else { "partially_refunded" },
                            "fully_refunded": fully_refunded,
                            "reason": request.reason,
                        }),
                    );
                }
            }
        } else {
            if let Some(mut stored) = transactions_store().get_mut(&id) {
                stored.status = "failed".to_string();
            }
            if let Some(original_id) = original_id {
                let was_waiting = transactions_store()
                    .get(&original_id)
                    .map(|entry| entry.value().status == "refund_pending")
                    .unwrap_or(false);
                if was_waiting {
                    let completed = completed_refunded_amount_minor(original_id);
                    if let Some(mut stored) = transactions_store().get_mut(&original_id) {
                        stored.status = if completed > 0 {
                            "partially_refunded".to_string()
                        } else {
                            "captured".to_string()
                        };
                    }
                }
            }
        }
        Ok(transactions_store()
            .get(&id)
            .map(|entry| entry.clone())
            .unwrap_or(refund_transaction))
    })();
    json::respond(result, debug)
}

/// Void an authorization (or an uncaptured charge) at the provider.
async fn void_transaction(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentTransaction> = (|| async {
        let id = id.into_inner();
        let original = transactions_store()
            .get(&id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Payment transaction not found"))?;
        require_customer(&req, &original.customer_id, "transaction")?;
        if original.kind != "authorize" && original.kind != "charge" {
            return Err(UnifiedError::unprocessable(format!(
                "Only an authorization or uncaptured charge can be voided (transaction kind is \"{}\")",
                original.kind
            )));
        }
        if original.status != "authorized" && original.status != "requires_confirmation" {
            return Err(UnifiedError::unprocessable(format!(
                "Only an authorized transaction can be voided (status is \"{}\")",
                original.status
            )));
        }
        let connection = find_connection(original.connection_id)?;
        let adapter = adapter_for(&connection)?;
        adapter.void(&original.provider_transaction_id).await?;
        let void_transaction = record_transaction(new_transaction(
            connection.id,
            &original.provider,
            "void",
            &original.customer_id,
            original.order_id.clone(),
            original.invoice_id.clone(),
            original.amount_minor,
            &original.currency,
            PaymentStatus::Voided,
            original.provider_transaction_id.clone(),
            Uuid::now_v7().to_string(),
        ));
        if let Some(mut stored) = transactions_store().get_mut(&id) {
            stored.status = "voided".to_string();
        }
        Ok(void_transaction)
    })()
    .await;
    match result {
        Ok(transaction) => json::success_response(transaction, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

/// Pay money out through a provider that supports payouts.
async fn create_payout(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreatePayoutRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentTransaction> = (|| async {
        let input = body.into_inner();
        validate_money(input.amount_minor, &input.currency)?;
        let connection = find_connection(input.connection_id)?;
        let key = idempotency_key(input.idempotency_key.clone());
        if let Some(existing) = find_by_idempotency(connection.id, &key) {
            return Ok(existing);
        }
        let adapter = adapter_for(&connection)?;
        let payout = adapter
            .payout(&PayoutInput {
                amount_minor: input.amount_minor,
                currency: input.currency.clone(),
                destination_account: input.destination_account.clone(),
                destination_type: input.destination_type.clone(),
                description: input.description.clone(),
                idempotency_key: key.clone(),
            })
            .await?;
        let transaction = record_transaction(new_transaction(
            connection.id,
            connection.kind.as_str(),
            "payout",
            &input.destination_account,
            None,
            None,
            input.amount_minor,
            &input.currency,
            payout.status,
            payout.provider_payout_id.clone(),
            key,
        ));
        // A payout is an outgoing movement; there is no order or
        // invoice to notify about, so no payment event is emitted.
        Ok(transaction)
    })()
    .await;
    match result {
        Ok(transaction) => json::success_response(transaction, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// Handlers: payment methods (tokenized only).
// ---------------------------------------------------------------------------

async fn create_payment_method(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    body: web::Json<CreatePaymentMethodRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentMethod> = (|| {
        let input = body.into_inner();
        if input.customer_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("customer_id is required"));
        }
        require_customer(&req, &input.customer_id, "customer")?;
        let token = validate_token(Some(&input.provider_token), "payment method")?;
        if input.brand.trim().is_empty() {
            return Err(UnifiedError::bad_request("Card brand is required"));
        }
        if input.last4.len() != 4 || !input.last4.chars().all(|c| c.is_ascii_digit()) {
            return Err(UnifiedError::bad_request(
                "last4 must be exactly four digits",
            ));
        }
        if !(1..=12).contains(&input.exp_month) {
            return Err(UnifiedError::bad_request("exp_month must be 1-12"));
        }
        // Coarse shape guard only: the year must be a plausible
        // four-digit year no tenant setting could ever accept
        // beyond (current year + the cards module's 50-year
        // maximum). Past-ness and the real per-tenant bound stay
        // the validator's `invalid_expiry` — the gate below runs
        // it. See `CARDS_EXPIRY_YEAR_GUARD_MAX_AHEAD`.
        let latest_expiry_year =
            Utc::now().year() + CARDS_EXPIRY_YEAR_GUARD_MAX_AHEAD as i32;
        if input.exp_year < 2000 || input.exp_year > latest_expiry_year as u32 {
            return Err(UnifiedError::bad_request(
                "exp_year must be a four-digit year no more than 50 years ahead",
            ));
        }
        let connection = find_connection(input.connection_id)?;
        // Batch 18 (S5): validate the claimed brand / last4 /
        // expiry before storing the method (`claim` mode when no
        // raw card details were supplied, `full` mode when they
        // were). The stored brand comes from the validator's
        // answer — authoritative — falling back to the claimed
        // brand when the answer carried none.
        let outcome = request_card_validation(
            &state,
            &req,
            input.card.as_ref(),
            None,
            Some(CardMethodRef {
                brand: input.brand.trim().to_string(),
                last4: input.last4.clone(),
                exp_month: input.exp_month,
                exp_year: input.exp_year,
            }),
            &connection,
            &input.customer_id,
            None,
            None,
        )?;
        let brand = outcome
            .as_ref()
            .and_then(|outcome| outcome.brand.clone())
            .unwrap_or_else(|| input.brand.trim().to_string());
        let method = PaymentMethod {
            id: Uuid::now_v7(),
            customer_id: input.customer_id.clone(),
            connection_id: connection.id,
            provider: connection.kind.as_str().to_string(),
            provider_token: token,
            brand,
            last4: input.last4,
            exp_month: input.exp_month,
            exp_year: input.exp_year,
            is_default: input.is_default,
            created_at: Utc::now(),
        };
        if method.is_default {
            for mut entry in methods_store().iter_mut() {
                if entry.value().customer_id == method.customer_id && entry.value().is_default {
                    entry.value_mut().is_default = false;
                }
            }
        }
        methods_store().insert(method.id, method.clone());
        Ok(method)
    })();
    match result {
        Ok(method) => json::success_response(method, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_payment_methods(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<PaymentMethod>> = (|| {
        let mut items: Vec<PaymentMethod> = methods_store()
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|method| {
                matches_search(&format!("{} {} {}", method.brand, method.last4, method.provider), &params)
                    && params
                        .filters
                        .get("customer_id")
                        .is_none_or(|customer| method.customer_id == *customer)
                    && params
                        .filters
                        .get("customer")
                        .is_none_or(|customer| method.customer_id == *customer)
                    && params
                        .filters
                        .get("provider")
                        .is_none_or(|provider| method.provider == *provider)
                    && params
                        .filters
                        .get("connection_id")
                        .is_none_or(|id| method.connection_id.to_string() == *id)
            })
            .collect();
        items.sort_by_key(|method| method.created_at);
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn get_payment_method(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentMethod> = (|| {
        let method = methods_store()
            .get(&id.into_inner())
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Payment method not found"))?;
        require_customer(&req, &method.customer_id, "payment method")?;
        Ok(method)
    })();
    json::respond(result, debug)
}

async fn delete_payment_method(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        let method = methods_store()
            .get(&id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Payment method not found"))?;
        require_customer(&req, &method.customer_id, "payment method")?;
        let in_use = subscriptions_store().iter().any(|entry| {
            entry.value().payment_method_id == Some(id) && entry.value().status == "active"
        });
        if in_use {
            return Err(UnifiedError::conflict(
                "Payment method is used by an active subscription; cancel the subscription first",
            ));
        }
        methods_store().remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

async fn set_default_payment_method(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentMethod> = (|| {
        let id = id.into_inner();
        let method = methods_store()
            .get(&id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Payment method not found"))?;
        require_customer(&req, &method.customer_id, "payment method")?;
        for mut entry in methods_store().iter_mut() {
            if entry.value().customer_id == method.customer_id {
                entry.value_mut().is_default = entry.key() == &id;
            }
        }
        // Return the stored record, not the pre-update clone, so
        // `is_default` reflects the change.
        methods_store()
            .get(&id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Payment method not found"))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: subscriptions.
// ---------------------------------------------------------------------------

/// One renewal attempt for a subscription: charge the stored token for
/// one period, advance the period on success, mark past-due and emit
/// `payment.failed` on failure. Shared by the renew endpoint and the
/// renew-due sweep.
async fn renew_one(
    state: &PaymentsState,
    subscription_id: Uuid,
) -> UnifiedResult<Uuid> {
    let subscription = subscriptions_store()
        .get(&subscription_id)
        .map(|entry| entry.clone())
        .ok_or_else(|| UnifiedError::not_found("Subscription not found"))?;
    if subscription.status != "active" && subscription.status != "past_due" {
        return Err(UnifiedError::conflict(format!(
            "Subscription in status \"{}\" cannot renew",
            subscription.status
        )));
    }
    let token = match subscription.payment_method_id {
        Some(method_id) => {
            let method = methods_store()
                .get(&method_id)
                .map(|entry| entry.clone())
                .ok_or_else(|| {
                    UnifiedError::not_found(format!(
                        "Stored payment method \"{method_id}\" for the subscription no longer exists"
                    ))
                })?;
            validate_token(Some(&method.provider_token), "stored method")?
        }
        None => {
            return Err(UnifiedError::bad_request(
                "Subscription has no stored payment method to renew with",
            ));
        }
    };
    let connection = find_connection(subscription.connection_id)?;
    let adapter = adapter_for(&connection)?;
    let key = Uuid::now_v7().to_string();
    let result = adapter
        .charge(&ChargeInput {
            amount_minor: subscription.amount_minor,
            currency: subscription.currency.clone(),
            customer_id: subscription.customer_id.clone(),
            order_id: subscription.order_id.clone(),
            reference_id: None,
            invoice_id: None,
            description: Some(format!(
                "Subscription renewal (plan {})",
                subscription.plan_id.clone().unwrap_or_default()
            )),
            payment_method_token: Some(token),
            idempotency_key: key.clone(),
            three_ds: false,
            billing: None,
        })
        .await;
    match result {
        Ok(charge) => {
            if charge.status != PaymentStatus::Captured && charge.status != PaymentStatus::Pending {
                return Err(renewal_failed(state, &subscription, "declined by provider"));
            }
            let mut transaction = new_transaction(
                connection.id,
                &subscription.provider,
                "subscription_charge",
                &subscription.customer_id,
                subscription.order_id.clone(),
                None,
                subscription.amount_minor,
                &subscription.currency,
                charge.status,
                charge.provider_transaction_id.clone(),
                key,
            );
            transaction.subscription_id = Some(subscription.id);
            transaction.fee_minor = charge.fee_minor;
            let transaction = record_transaction(transaction);
            let next_end = period_end(
                &subscription.interval,
                subscription.interval_count,
                0,
            );
            if let Some(mut stored) = subscriptions_store().get_mut(&subscription_id) {
                stored.status = "active".to_string();
                stored.current_period_end = Some(next_end);
                stored.next_renewal_at = Some(next_end);
                stored.updated_at = Utc::now();
            }
            state.emit(
                PAYMENT_SUBSCRIPTION_RENEWED,
                json!({
                    "subscription_id": subscription.id,
                    "transaction_id": transaction.id,
                    "provider_subscription_id": subscription.provider_subscription_id,
                    "provider": subscription.provider,
                    "connection_id": connection.id,
                    "customer_id": subscription.customer_id,
                    "order_id": subscription.order_id,
                    "amount_minor": subscription.amount_minor,
                    "currency": subscription.currency,
                    "fee_minor": charge.fee_minor,
                    "current_period_end": next_end.to_rfc3339(),
                }),
            );
            state.emit(
                PAYMENT_CAPTURED,
                json!({
                    "transaction_id": transaction.id,
                    "provider_transaction_id": charge.provider_transaction_id,
                    "provider": subscription.provider,
                    "connection_id": connection.id,
                    "customer_id": subscription.customer_id,
                    "order_id": subscription.order_id,
                    "subscription_id": subscription.id,
                    "amount_minor": subscription.amount_minor,
                    "currency": subscription.currency,
                    "fee_minor": charge.fee_minor,
                    "status": transaction.status,
                }),
            );
            Ok(transaction.id)
        }
        Err(error) => Err(renewal_failed(state, &subscription, &error.message)),
    }
}

/// Mark a subscription past-due, emit `payment.failed`, and notify the
/// customer. Returns the error the renew endpoint reports.
fn renewal_failed(
    state: &PaymentsState,
    subscription: &GatewaySubscription,
    reason: &str,
) -> UnifiedError {
    if let Some(mut stored) = subscriptions_store().get_mut(&subscription.id) {
        stored.status = "past_due".to_string();
        stored.updated_at = Utc::now();
    }
    state.emit(
        PAYMENT_FAILED,
        json!({
            "subscription_id": subscription.id,
            "provider_subscription_id": subscription.provider_subscription_id,
            "provider": subscription.provider,
            "customer_id": subscription.customer_id,
            "order_id": subscription.order_id,
            "amount_minor": subscription.amount_minor,
            "currency": subscription.currency,
            "reason": reason,
        }),
    );
    notify(
        state,
        &subscription.customer_id,
        "payment_failed",
        "Subscription renewal failed",
        &format!(
            "The renewal of your subscription ({} {}) failed: {reason}. Please update your payment method.",
            subscription.amount_minor, subscription.currency
        ),
    );
    UnifiedError::bad_request(format!("Subscription renewal failed: {reason}"))
        .with_metadata("provider", Value::String(subscription.provider.clone()))
        .with_metadata(
            "subscription_id",
            Value::String(subscription.id.to_string()),
        )
}

async fn create_subscription(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    body: web::Json<CreateSubscriptionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let input = body.into_inner();
        if input.customer_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("customer_id is required"));
        }
        validate_money(input.amount_minor, &input.currency)?;
        if !matches!(input.interval.as_str(), "day" | "week" | "month" | "year") {
            return Err(UnifiedError::bad_request(
                "Subscription interval must be day, week, month, or year",
            ));
        }
        if input.interval_count == 0 {
            return Err(UnifiedError::bad_request(
                "interval_count must be at least 1",
            ));
        }
        let connection = find_connection(input.connection_id)?;
        let token = resolve_token(
            input.payment_method_token.as_deref(),
            input.payment_method_id,
            &input.customer_id,
        )?;
        // Batch 18 (S6): validate the card before the provider
        // call — `subscription_create` can charge immediately.
        // Raw details run in `full` mode; a stored method id runs
        // in `method` mode; an opaque token alone skips the gate.
        // NOTE (pre-existing gap, not fixed here): this handler
        // has no `require_customer` ownership check.
        request_card_validation(&state, &req, input.card.as_ref(), input.payment_method_id, None, &connection, &input.customer_id, input.order_id.as_deref(), None)?;
        let adapter = adapter_for(&connection)?;
        let key = idempotency_key(input.idempotency_key.clone());
        let created = adapter
            .subscription_create(&SubscriptionInput {
                customer_id: input.customer_id.clone(),
                plan_id: input.plan_id.clone(),
                amount_minor: input.amount_minor,
                currency: input.currency.clone(),
                interval: input.interval.clone(),
                interval_count: input.interval_count,
                trial_days: input.trial_days,
                payment_method_token: token,
                order_id: input.order_id.clone(),
                idempotency_key: key,
            })
            .await?;
        let period_end_at = created
            .current_period_end
            .unwrap_or_else(|| period_end(&input.interval, input.interval_count, input.trial_days));
        // A provider status that is not yet active (PayPal
        // APPROVAL_PENDING, for example) is recorded as pending, not
        // active: the subscription only renews once it is active.
        let status = match created.status.to_lowercase().as_str() {
            "active" | "trialing" | "succeeded" => "active".to_string(),
            other => other.to_string(),
        };
        let now = Utc::now();
        let subscription = GatewaySubscription {
            id: Uuid::now_v7(),
            connection_id: connection.id,
            provider: connection.kind.as_str().to_string(),
            customer_id: input.customer_id.clone(),
            order_id: input.order_id.clone(),
            plan_id: input.plan_id.clone(),
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
            interval: input.interval.clone(),
            interval_count: input.interval_count,
            status: status.clone(),
            provider_subscription_id: created.provider_subscription_id.clone(),
            payment_method_id: input.payment_method_id,
            current_period_end: Some(period_end_at),
            next_renewal_at: Some(period_end_at),
            cancelled_at: None,
            created_at: now,
            updated_at: now,
        };
        subscriptions_store().insert(subscription.id, subscription.clone());
        state.emit(
            PAYMENT_SUBSCRIPTION_CREATED,
            json!({
                "subscription_id": subscription.id,
                "provider_subscription_id": created.provider_subscription_id,
                "provider": subscription.provider,
                "connection_id": connection.id,
                "customer_id": input.customer_id,
                "order_id": input.order_id,
                "plan_id": input.plan_id,
                "amount_minor": input.amount_minor,
                "currency": input.currency,
                "interval": input.interval,
                "status": subscription.status,
                "current_period_end": period_end_at.to_rfc3339(),
            }),
        );
        // Ask the scheduler to run the renewal at the period end.
        // The scheduler rejects the kind until it learns it, so the
        // outcome is reported honestly in the response.
        let renewal_enqueued = enqueue_renewal(&state, subscription.id, period_end_at);
        Ok(json!({
            "subscription": subscription,
            "renewal_enqueued": renewal_enqueued,
            "renewal_note": if renewal_enqueued {
                Value::Null
            } else {
                Value::String("Scheduler does not know the payment.subscription.renew job kind yet; renewals can be triggered with POST /api/v1/payments/subscriptions/renew-due until the scheduler learns it".to_string())
            },
        }))
    })()
    .await;
    match result {
        Ok(value) => json::success_response(value, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_subscriptions(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<GatewaySubscription>> = (|| {
        let mut items: Vec<GatewaySubscription> = subscriptions_store()
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|subscription| {
                matches_search(
                    &format!(
                        "{} {} {}",
                        subscription.provider_subscription_id,
                        subscription.provider,
                        subscription.customer_id
                    ),
                    &params,
                ) && params
                    .filters
                    .get("provider")
                    .is_none_or(|provider| subscription.provider == *provider)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| subscription.status == *status)
                    && params
                        .filters
                        .get("customer_id")
                        .is_none_or(|customer| subscription.customer_id == *customer)
                    && params
                        .filters
                        .get("customer")
                        .is_none_or(|customer| subscription.customer_id == *customer)
                    && params
                        .filters
                        .get("order_id")
                        .is_none_or(|order_id| subscription.order_id.as_deref() == Some(order_id.as_str()))
            })
            .collect();
        items.sort_by_key(|subscription| subscription.created_at);
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn get_subscription(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GatewaySubscription> = (|| {
        subscriptions_store()
            .get(&id.into_inner())
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Subscription not found"))
    })();
    json::respond(result, debug)
}

async fn cancel_subscription(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    id: web::Path<Uuid>,
    body: web::Json<CancelSubscriptionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GatewaySubscription> = (|| async {
        let id = id.into_inner();
        let request = body.into_inner();
        let mut subscription = subscriptions_store()
            .get(&id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Subscription not found"))?;
        if subscription.status == "cancelled" || subscription.status == "ended" {
            return Err(UnifiedError::conflict(format!(
                "Subscription is already {}",
                subscription.status
            )));
        }
        let connection = find_connection(subscription.connection_id)?;
        let adapter = adapter_for(&connection)?;
        adapter
            .subscription_cancel(&subscription.provider_subscription_id)
            .await?;
        subscription.status = "cancelled".to_string();
        subscription.cancelled_at = Some(Utc::now());
        subscription.updated_at = Utc::now();
        subscriptions_store().insert(id, subscription.clone());
        state.emit(
            PAYMENT_SUBSCRIPTION_CANCELLED,
            json!({
                "subscription_id": subscription.id,
                "provider_subscription_id": subscription.provider_subscription_id,
                "provider": subscription.provider,
                "customer_id": subscription.customer_id,
                "order_id": subscription.order_id,
                "reason": request.reason,
            }),
        );
        Ok(subscription)
    })()
    .await;
    json::respond(result, debug)
}

async fn renew_subscription(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let transaction_id = renew_one(&state, id.into_inner()).await?;
        let transaction = transactions_store()
            .get(&transaction_id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::internal("Renewal transaction vanished after charge"))?;
        Ok(json!({ "transaction": transaction }))
    })()
    .await;
    json::respond(result, debug)
}

/// Renew every subscription whose renewal time has arrived. This is
/// the endpoint a scheduler job or an operator cron calls until the
/// scheduler learns the `payment.subscription.renew` job kind.
async fn renew_due_subscriptions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let now = Utc::now();
        let due: Vec<Uuid> = subscriptions_store()
            .iter()
            .filter(|entry| {
                (entry.value().status == "active" || entry.value().status == "past_due")
                    && entry
                        .value()
                        .next_renewal_at
                        .is_some_and(|renew_at| renew_at <= now)
            })
            .map(|entry| *entry.key())
            .collect();
        let mut renewed: Vec<String> = Vec::new();
        let mut failed: Vec<Value> = Vec::new();
        for subscription_id in due {
            match renew_one(&state, subscription_id).await {
                Ok(transaction_id) => renewed.push(transaction_id.to_string()),
                Err(error) => failed.push(json!({
                    "subscription_id": subscription_id,
                    "error": error.message,
                })),
            }
        }
        Ok(json!({
            "renewed": renewed,
            "failed": failed,
            "checked_at": now.to_rfc3339(),
        }))
    })()
    .await;
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: store checkout through the gateway.
// ---------------------------------------------------------------------------

/// Pay for a store order through the gateway: charge once, record the
/// transaction, and emit `payment.captured` so the store can mark the
/// order paid / completed and billing can record the payment. The
/// store itself owns the order; this endpoint takes the amount the
/// caller computed for the order (the store's checkout total).
async fn pay_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    order_id: web::Path<String>,
    body: web::Json<PayOrderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentTransaction> = (|| async {
        let order_id = order_id.into_inner();
        if order_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("order_id is required"));
        }
        let input = body.into_inner();
        if input.customer_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("customer_id is required"));
        }
        require_customer(&req, &input.customer_id, "customer")?;
        validate_money(input.amount_minor, &input.currency)?;
        check_pending_checkout_amount(
            Some(order_id.as_str()),
            input.amount_minor,
            &input.currency,
        )?;
        let connection = find_connection(input.connection_id)?;
        let key = idempotency_key(input.idempotency_key.clone());
        if let Some(existing) = find_by_idempotency(connection.id, &key) {
            return Ok(existing);
        }
        let token = resolve_token(
            input.payment_method_token.as_deref(),
            input.payment_method_id,
            &input.customer_id,
        )?;
        // Batch 18 (S3): validate the card before any provider
        // call. Raw details on the request run in `full` mode; a
        // stored method id runs in `method` mode; an opaque token
        // alone skips the gate.
        request_card_validation(&state, &req, input.card.as_ref(), input.payment_method_id, None, &connection, &input.customer_id, Some(order_id.as_str()), None)?;
        let adapter = adapter_for(&connection)?;
        let result = adapter
            .charge(&ChargeInput {
                amount_minor: input.amount_minor,
                currency: input.currency.clone(),
                customer_id: input.customer_id.clone(),
                order_id: Some(order_id.clone()),
                reference_id: pending_checkout_reference(Some(order_id.as_str())),
                invoice_id: input.invoice_id.clone(),
                description: input.description.clone(),
                payment_method_token: token,
                idempotency_key: key.clone(),
                three_ds: input.three_ds,
                billing: input.billing.map(|billing| BillingDetails {
                    name: billing.name,
                    email: billing.email,
                    country: billing.country,
                    city: billing.city,
                    address: billing.address,
                }),
            })
            .await?;
        let mut transaction = new_transaction(
            connection.id,
            connection.kind.as_str(),
            "charge",
            &input.customer_id,
            Some(order_id.clone()),
            input.invoice_id.clone(),
            input.amount_minor,
            &input.currency,
            result.status,
            result.provider_transaction_id.clone(),
            key,
        );
        transaction.fee_minor = result.fee_minor;
        let transaction = record_transaction(transaction);
        let payload = json!({
            "transaction_id": transaction.id,
            "provider_transaction_id": result.provider_transaction_id,
            "provider": connection.kind.as_str(),
            "connection_id": connection.id,
            "customer_id": input.customer_id,
            "order_id": order_id,
            "invoice_id": input.invoice_id,
            "amount_minor": input.amount_minor,
            "currency": input.currency,
            "fee_minor": result.fee_minor,
            "status": transaction.status,
            "pending_checkout": pending_checkouts_store().contains_key(&order_id),
        });
        match result.status {
            PaymentStatus::Captured => state.emit(PAYMENT_CAPTURED, payload),
            PaymentStatus::Failed => {
                state.emit(PAYMENT_FAILED, payload);
                notify(
                    &state,
                    &input.customer_id,
                    "payment_failed",
                    "Payment failed",
                    &format!("Your payment for order {order_id} could not be completed."),
                );
            }
            _ => {}
        }
        if result.status == PaymentStatus::Failed {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" declined the payment for order {order_id}",
                connection.kind.as_str()
            ))
            .with_metadata("provider", Value::String(connection.kind.as_str().to_string()))
            .with_metadata("transaction_id", Value::String(transaction.id.to_string())));
        }
        Ok(transaction)
    })()
    .await;
    match result {
        Ok(transaction) => json::success_response(transaction, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// Handlers: webhooks.
// ---------------------------------------------------------------------------

/// One processed webhook event, for the webhook event list.
#[derive(Debug, Clone, Serialize)]
struct WebhookEventView {
    event_id: String,
    received_at: DateTime<Utc>,
}

/// What a verified webhook event means, after the server-side
/// outcome (when the provider offers one) and the body's own status
/// fields have been consulted.
enum WebhookDisposition {
    Chargeback,
    Refund(RefundDisposition),
    Failed,
    Captured,
    Ignored(&'static str),
}

/// What a webhook refund event actually says. Read from the event's
/// own status fields, never from the event type alone: a
/// `refund.failed` event must not mark anything refunded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefundDisposition {
    Completed,
    Partial,
    Pending,
    Failed,
}

/// Read the refund disposition from the event's status fields
/// (`data.object.status`, `status`, `refund_status`), falling back
/// to the event type only when the event carries no usable status.
fn webhook_refund_disposition(event: &Value, event_type: &str) -> RefundDisposition {
    let raw = event
        .get("data")
        .and_then(|data| data.get("object"))
        .and_then(|object| json_str(object, &["status", "refund_status"]))
        .or_else(|| json_str(event, &["status", "refund_status"]))
        .map(str::to_lowercase);
    let text = raw.as_deref().unwrap_or("");
    if text.contains("pending") || text.contains("processing") {
        return RefundDisposition::Pending;
    }
    if text.contains("fail")
        || text.contains("cancel")
        || text.contains("denied")
        || text.contains("reversed")
    {
        return RefundDisposition::Failed;
    }
    if text.contains("succeed") || text.contains("complete") || text.contains("refunded") {
        return RefundDisposition::Completed;
    }
    let lower = event_type.to_lowercase();
    if lower.contains("fail") || lower.contains("cancel") {
        RefundDisposition::Failed
    } else if lower.contains("pending") || lower.contains("processing") {
        RefundDisposition::Pending
    } else {
        RefundDisposition::Completed
    }
}

/// The currency an event claims, when it carries one.
fn event_currency(event: &Value) -> Option<String> {
    event
        .get("data")
        .and_then(|data| data.get("object"))
        .and_then(|object| json_str(object, &["currency"]))
        .or_else(|| json_str(event, &["currency"]))
        .map(str::to_string)
}

/// A minor-unit amount from the event, when the event carries one in
/// units this module can trust: the `amount_minor` key (this
/// module's own convention), or Stripe's `data.object.amount`,
/// which is minor units. Other providers report major units or
/// provider-specific shapes, so their amounts are never compared.
fn event_amount_minor(event: &Value, kind: PaymentProviderKind) -> Option<i64> {
    if let Some(amount) = json_i64(event, &["amount_minor"]) {
        return Some(amount);
    }
    if kind == PaymentProviderKind::Stripe {
        return event
            .get("data")
            .and_then(|data| data.get("object"))
            .and_then(|object| json_i64(object, &["amount"]));
    }
    None
}

/// Receive a webhook for one provider. The signature is verified
/// against every enabled connection of that kind before any
/// processing; the first connection whose secret verifies wins, so one
/// endpoint serves both the sandbox and the live connection of a
/// provider. A redelivered event id is acknowledged but not processed
/// twice.
async fn webhook_receiver(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    provider: web::Path<String>,
    req: HttpRequest,
    body: web::Bytes,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let kind = PaymentProviderKind::from_str(&provider).ok_or_else(|| {
            UnifiedError::not_found(format!("Unknown payment provider \"{provider}\""))
        })?;
        let raw_body = body.as_ref();
        let event: Value = serde_json::from_slice(raw_body).map_err(|_| {
            UnifiedError::bad_request("Webhook body is not valid JSON")
        })?;
        // Verify against every enabled connection of this kind.
        let mut verified_connection: Option<PaymentConnection> = None;
        for entry in connections_store().iter() {
            let connection = entry.value().clone();
            if connection.kind != kind || !connection.enabled {
                continue;
            }
            let adapter = payment_registry().build(&connection)?;
            match adapter.webhook_verify(req.headers(), raw_body).await {
                Ok(true) => {
                    verified_connection = Some(connection);
                    break;
                }
                Ok(false) => continue,
                Err(_) => continue,
            }
        }
        let connection = verified_connection.ok_or_else(|| {
            UnifiedError::unauthorized(format!(
                "Webhook signature verification failed for provider \"{}\"",
                kind.as_str()
            ))
        })?;
        // The event id is the provider's id when the body carries
        // one. Without one, the id is a hash of the raw body, so an
        // identical redelivery still deduplicates instead of getting
        // a fresh id (and a fresh application) every time.
        let event_id = json_str(&event, &["id", "event_id"])
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    "{}-sha256-{}",
                    kind.as_str(),
                    hex_encode(&Sha256::digest(raw_body))
                )
            });
        let event_type = json_str(&event, &["type", "event_type", "event"])
            .unwrap_or("unknown")
            .to_string();
        let duplicate = webhook_events_store().contains_key(&event_id);
        if !duplicate {
            webhook_events_store().insert(event_id.clone(), Utc::now());
            prune_webhook_events();
        }
        state.emit(
            PAYMENT_WEBHOOK_RECEIVED,
            json!({
                "provider": kind.as_str(),
                "connection_id": connection.id,
                "event_id": event_id,
                "event_type": event_type,
                "duplicate": duplicate,
            }),
        );
        if duplicate {
            return Ok(json!({
                "received": true,
                "duplicate": true,
                "event_id": event_id,
            }));
        }
        // Providers without signed webhooks (HyperPay, 2Checkout) are
        // verified by re-fetching the resource; the body's event type
        // is unauthenticated there, so the outcome is read from the
        // provider instead. A failed confirmation fails closed: the
        // webhook is rejected without any state change.
        let adapter = payment_registry().build(&connection)?;
        let verified_outcome = adapter.webhook_outcome(raw_body).await?;
        // Map the event onto a transaction when the body names one,
        // and emit the type-specific event. The provider transaction
        // id is looked up in the usual nested places first.
        let provider_transaction_id = event
            .get("data")
            .and_then(|data| data.get("object"))
            .and_then(|object| json_str(object, &["id", "payment_intent", "charge"]))
            .or_else(|| {
                event
                    .get("resource")
                    .and_then(|resource| json_str(resource, &["id", "parent_payment"]))
            })
            .or_else(|| json_str(&event, &["transaction_id", "order_id", "charge_id", "payment_id"]))
            .map(str::to_string);
        let matched = provider_transaction_id
            .as_deref()
            .and_then(stored_transaction);
        let event_lower = event_type.to_lowercase();
        let customer_id = matched
            .as_ref()
            .map(|transaction| transaction.customer_id.clone())
            .unwrap_or_default();
        // Decide what the event means. A server-side outcome wins
        // over the body's event type; otherwise the event type string
        // decides, with the refund disposition read from the event's
        // own status fields (a `refund.failed` event must never mark
        // anything refunded).
        let disposition = match verified_outcome {
            Some(PaymentStatus::Captured) => WebhookDisposition::Captured,
            Some(PaymentStatus::Refunded) => {
                WebhookDisposition::Refund(RefundDisposition::Completed)
            }
            Some(PaymentStatus::PartiallyRefunded) => {
                WebhookDisposition::Refund(RefundDisposition::Partial)
            }
            Some(PaymentStatus::Pending) => WebhookDisposition::Ignored("pending"),
            Some(PaymentStatus::Chargeback) => WebhookDisposition::Chargeback,
            Some(
                PaymentStatus::Failed
                | PaymentStatus::Cancelled
                | PaymentStatus::Voided,
            ) => WebhookDisposition::Failed,
            Some(_) => WebhookDisposition::Ignored("authorized"),
            None if event_lower.contains("chargeback") || event_lower.contains("dispute") => {
                WebhookDisposition::Chargeback
            }
            None if event_lower.contains("refund") => {
                WebhookDisposition::Refund(webhook_refund_disposition(&event, &event_type))
            }
            None if event_lower.contains("fail") || event_lower.contains("declined") => {
                WebhookDisposition::Failed
            }
            None
                if event_lower.contains("succeed")
                    || event_lower.contains("captured")
                    || event_lower.contains("completed")
                    || event_lower.contains("settlement") =>
            {
                WebhookDisposition::Captured
            }
            None => WebhookDisposition::Ignored("unknown event type"),
        };
        let mut outcome = "ignored";
        match disposition {
            WebhookDisposition::Chargeback => {
                if let Some(transaction) = &matched {
                    if let Some(mut stored) = transactions_store().get_mut(&transaction.id) {
                        stored.status = "chargeback".to_string();
                    }
                }
                state.emit(
                    PAYMENT_CHARGEBACK,
                    json!({
                        "provider": kind.as_str(),
                        "connection_id": connection.id,
                        "event_id": event_id,
                        "event_type": event_type,
                        "transaction_id": matched.as_ref().map(|t| t.id),
                        "provider_transaction_id": provider_transaction_id,
                        "customer_id": customer_id,
                        "order_id": matched.as_ref().and_then(|t| t.order_id.clone()),
                        "invoice_id": matched.as_ref().and_then(|t| t.invoice_id.clone()),
                        "amount_minor": matched.as_ref().map(|t| t.amount_minor),
                        "currency": matched.as_ref().map(|t| t.currency.clone()),
                    }),
                );
                notify(
                    &state,
                    &customer_id,
                    "payment_chargeback",
                    "Chargeback received",
                    "A chargeback was filed for one of your payments. The order and license are under review.",
                );
                outcome = "chargeback";
            }
            WebhookDisposition::Refund(refund_disposition) => {
                match refund_disposition {
                    RefundDisposition::Completed | RefundDisposition::Partial => {
                        // A completed refund is only full when the
                        // event carries no smaller minor-unit amount.
                        let mut fully_refunded =
                            refund_disposition == RefundDisposition::Completed;
                        if let Some(amount) = event_amount_minor(&event, kind) {
                            if let Some(transaction) = &matched {
                                if amount < transaction.amount_minor {
                                    fully_refunded = false;
                                }
                            }
                        }
                        let applied = matches!(
                            matched.as_ref().map(|t| t.status.as_str()),
                            Some("captured")
                                | Some("partially_refunded")
                                | Some("refund_pending")
                        );
                        if applied {
                            if let Some(transaction) = &matched {
                                if let Some(mut stored) =
                                    transactions_store().get_mut(&transaction.id)
                                {
                                    stored.status = if fully_refunded {
                                        "refunded".to_string()
                                    } else {
                                        "partially_refunded".to_string()
                                    };
                                }
                            }
                            state.emit(
                                PAYMENT_REFUNDED,
                                json!({
                                    "provider": kind.as_str(),
                                    "connection_id": connection.id,
                                    "event_id": event_id,
                                    "event_type": event_type,
                                    "transaction_id": matched.as_ref().map(|t| t.id),
                                    "provider_transaction_id": provider_transaction_id,
                                    "customer_id": customer_id,
                                    "order_id": matched.as_ref().and_then(|t| t.order_id.clone()),
                                    "invoice_id": matched.as_ref().and_then(|t| t.invoice_id.clone()),
                                    "amount_minor": event_amount_minor(&event, kind)
                                        .or_else(|| matched.as_ref().map(|t| t.amount_minor)),
                                    "currency": matched.as_ref().map(|t| t.currency.clone()),
                                    "status": if fully_refunded { "refunded" } else { "partially_refunded" },
                                    "fully_refunded": fully_refunded,
                                    "source": "webhook",
                                }),
                            );
                            outcome = if fully_refunded {
                                "refunded"
                            } else {
                                "partially_refunded"
                            };
                        } else {
                            outcome = "refund_ignored";
                        }
                    }
                    RefundDisposition::Pending => {
                        // A refund is in flight: the charge waits in
                        // `refund_pending` until the refund completes
                        // or fails. The refunded event carries a
                        // pending status, so listeners never mark the
                        // order or invoice refunded on it.
                        if let Some(transaction) = &matched {
                            if matches!(
                                transaction.status.as_str(),
                                "captured" | "partially_refunded"
                            ) {
                                if let Some(mut stored) =
                                    transactions_store().get_mut(&transaction.id)
                                {
                                    stored.status = "refund_pending".to_string();
                                }
                            }
                        }
                        state.emit(
                            PAYMENT_REFUNDED,
                            json!({
                                "provider": kind.as_str(),
                                "connection_id": connection.id,
                                "event_id": event_id,
                                "event_type": event_type,
                                "transaction_id": matched.as_ref().map(|t| t.id),
                                "provider_transaction_id": provider_transaction_id,
                                "customer_id": customer_id,
                                "order_id": matched.as_ref().and_then(|t| t.order_id.clone()),
                                "invoice_id": matched.as_ref().and_then(|t| t.invoice_id.clone()),
                                "amount_minor": event_amount_minor(&event, kind)
                                    .or_else(|| matched.as_ref().map(|t| t.amount_minor)),
                                "currency": matched.as_ref().map(|t| t.currency.clone()),
                                "status": "pending",
                                "fully_refunded": false,
                                "source": "webhook",
                            }),
                        );
                        outcome = "refund_pending";
                    }
                    RefundDisposition::Failed => {
                        // The refund died: a charge that was waiting
                        // in `refund_pending` goes back to captured.
                        if let Some(transaction) = &matched {
                            if transaction.status == "refund_pending" {
                                if let Some(mut stored) =
                                    transactions_store().get_mut(&transaction.id)
                                {
                                    stored.status = "captured".to_string();
                                }
                            }
                        }
                        outcome = "refund_failed";
                    }
                }
            }
            WebhookDisposition::Failed => {
                // Only an open transaction can fail: a capture that
                // already succeeded is never rewritten to failed by a
                // later event.
                let applied = matches!(
                    matched.as_ref().map(|t| t.status.as_str()),
                    Some("authorized") | Some("requires_confirmation") | Some("pending")
                );
                if applied {
                    if let Some(transaction) = &matched {
                        if let Some(mut stored) = transactions_store().get_mut(&transaction.id) {
                            stored.status = "failed".to_string();
                        }
                    }
                    notify(
                        &state,
                        &customer_id,
                        "payment_failed",
                        "Payment failed",
                        "A payment failed. Please check your payment method and try again.",
                    );
                }
                state.emit(
                    PAYMENT_FAILED,
                    json!({
                        "provider": kind.as_str(),
                        "connection_id": connection.id,
                        "event_id": event_id,
                        "event_type": event_type,
                        "transaction_id": matched.as_ref().map(|t| t.id),
                        "provider_transaction_id": provider_transaction_id,
                        "customer_id": customer_id,
                        "order_id": matched.as_ref().and_then(|t| t.order_id.clone()),
                        "invoice_id": matched.as_ref().and_then(|t| t.invoice_id.clone()),
                    }),
                );
                outcome = "failed";
            }
            WebhookDisposition::Captured => {
                // Amount and currency guards: a capture whose currency
                // differs from the stored transaction, or whose
                // minor-unit amount is below it, is not applied. A
                // partial capture leaves the transaction alone, and a
                // transaction that already reached a terminal state is
                // never rewritten to captured.
                let currency_mismatch = event_currency(&event)
                    .zip(matched.as_ref().map(|t| t.currency.clone()))
                    .is_some_and(|(event_currency, stored_currency)| {
                        !event_currency.eq_ignore_ascii_case(&stored_currency)
                    });
                let underpaid = event_amount_minor(&event, kind)
                    .zip(matched.as_ref().map(|t| t.amount_minor))
                    .is_some_and(|(amount, stored)| amount < stored);
                let terminal = matches!(
                    matched.as_ref().map(|t| t.status.as_str()),
                    Some("refunded")
                        | Some("partially_refunded")
                        | Some("chargeback")
                        | Some("voided")
                );
                if currency_mismatch {
                    outcome = "currency_mismatch";
                } else if underpaid {
                    outcome = "partial_capture";
                } else if terminal {
                    outcome = "already_terminal";
                } else {
                    if let Some(transaction) = &matched {
                        if let Some(mut stored) = transactions_store().get_mut(&transaction.id) {
                            stored.status = "captured".to_string();
                        }
                    }
                    state.emit(
                        PAYMENT_CAPTURED,
                        json!({
                            "provider": kind.as_str(),
                            "connection_id": connection.id,
                            "event_id": event_id,
                            "event_type": event_type,
                            "transaction_id": matched.as_ref().map(|t| t.id),
                            "provider_transaction_id": provider_transaction_id,
                            "customer_id": customer_id,
                            "order_id": matched.as_ref().and_then(|t| t.order_id.clone()),
                            "invoice_id": matched.as_ref().and_then(|t| t.invoice_id.clone()),
                            "amount_minor": matched.as_ref().map(|t| t.amount_minor),
                            "currency": matched.as_ref().map(|t| t.currency.clone()),
                            "status": "captured",
                            "source": "webhook",
                        }),
                    );
                    outcome = "captured";
                }
            }
            WebhookDisposition::Ignored(reason) => {
                outcome = reason;
            }
        }
        Ok(json!({
            "received": true,
            "duplicate": false,
            "event_id": event_id,
            "event_type": event_type,
            "provider": kind.as_str(),
            "outcome": outcome,
        }))
    })()
    .await;
    json::respond(result, debug)
}

async fn list_webhook_events(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<WebhookEventView>> = (|| {
        let mut items: Vec<WebhookEventView> = webhook_events_store()
            .iter()
            .map(|entry| WebhookEventView {
                event_id: entry.key().clone(),
                received_at: *entry.value(),
            })
            .collect();
        items.sort_by_key(|event| event.received_at);
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: split bills.
// ---------------------------------------------------------------------------

/// Recompute the paid / remaining amounts of a split from its parts.
fn recompute_split_totals(split: &mut SplitBill) {
    split.paid_minor = split
        .parts
        .iter()
        .filter(|part| part.status == "paid")
        .map(|part| part.amount_minor)
        .sum();
    split.remaining_minor = (split.total_minor - split.paid_minor).max(0);
    split.updated_at = Utc::now();
}

fn find_split(id: Uuid) -> UnifiedResult<SplitBill> {
    splits_store()
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Split bill \"{id}\" not found")))
}

/// Validate a create-split request and build the record. The parts
/// must sum exactly to the total: a mismatch is a 422 whose metadata
/// carries the total, the parts sum, the difference, and the per-part
/// amounts, so the caller can see which part is wrong.
///
/// Note: the caller supplies `total_minor` (the order/invoice total
/// in minor units). This module cannot read the store or billing
/// totals directly — hook wiring only — so the integrator should
/// validate the total against the order/invoice when the durable
/// layer lands (see the coordinator report).
fn build_split_bill(input: CreateSplitRequest) -> UnifiedResult<SplitBill> {
    let customer_id = input.customer_id.trim().to_string();
    if customer_id.is_empty() {
        return Err(UnifiedError::bad_request("customer_id is required"));
    }
    if input.total_minor <= 0 {
        return Err(UnifiedError::bad_request(
            "Split total must be greater than zero (integer minor units)",
        ));
    }
    if input.total_minor > MAX_AMOUNT_MINOR {
        return Err(UnifiedError::bad_request(
            "Split total exceeds the maximum accepted for one charge",
        ));
    }
    let currency = validate_currency(&input.currency)?;
    let order_id = input
        .order_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let invoice_id = input
        .invoice_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if order_id.is_none() && invoice_id.is_none() {
        return Err(UnifiedError::bad_request(
            "A split needs order_id or invoice_id",
        ));
    }
    if input.parts.len() < 2 {
        return Err(UnifiedError::unprocessable(
            "A split needs at least two parts",
        ));
    }
    let split_id = Uuid::now_v7();
    let mut parts: Vec<SplitPart> = Vec::with_capacity(input.parts.len());
    let mut parts_sum: i64 = 0;
    for (index, part_input) in input.parts.iter().enumerate() {
        if part_input.amount_minor <= 0 {
            return Err(UnifiedError::bad_request(format!(
                "Split part {index} amount must be greater than zero"
            )));
        }
        let provider = part_input
            .provider
            .clone()
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        let kind = PaymentProviderKind::from_str(&provider).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Split part {index} has unknown provider \"{provider}\""
            ))
        })?;
        let payer_id = part_input
            .payer_id
            .clone()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| customer_id.clone());
        parts_sum += part_input.amount_minor;
        parts.push(SplitPart {
            id: Uuid::now_v7(),
            split_id,
            index,
            amount_minor: part_input.amount_minor,
            currency: currency.clone(),
            provider: kind.as_str().to_string(),
            connection_id: part_input.connection_id,
            payer_id,
            due_date: part_input.due_date,
            status: "pending".to_string(),
            paid_transaction_id: None,
            paid_at: None,
        });
    }
    if parts_sum != input.total_minor {
        return Err(UnifiedError::unprocessable(format!(
            "Split parts sum to {parts_sum} {currency} but the total is {} {currency}",
            input.total_minor
        ))
        .with_metadata("total_minor", Value::from(input.total_minor))
        .with_metadata("parts_sum_minor", Value::from(parts_sum))
        .with_metadata(
            "difference_minor",
            Value::from(input.total_minor - parts_sum),
        )
        .with_metadata(
            "parts",
            Value::Array(
                parts
                    .iter()
                    .map(|part| {
                        json!({
                            "index": part.index,
                            "amount_minor": part.amount_minor,
                            "provider": part.provider.clone(),
                            "payer_id": part.payer_id.clone(),
                        })
                    })
                    .collect(),
            ),
        ));
    }
    let now = Utc::now();
    let mut split = SplitBill {
        id: split_id,
        order_id,
        invoice_id,
        customer_id,
        total_minor: input.total_minor,
        currency,
        status: "active".to_string(),
        parts,
        paid_minor: 0,
        remaining_minor: input.total_minor,
        created_at: now,
        updated_at: now,
    };
    recompute_split_totals(&mut split);
    Ok(split)
}

async fn create_split(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    body: web::Json<CreateSplitRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SplitBill> = (|| {
        let split = build_split_bill(body.into_inner())?;
        splits_store().insert(split.id, split.clone());
        state.emit(
            PAYMENT_SPLIT_CREATED,
            json!({
                "split_id": split.id,
                "order_id": split.order_id.clone(),
                "invoice_id": split.invoice_id.clone(),
                "customer_id": split.customer_id.clone(),
                "total_minor": split.total_minor,
                "currency": split.currency.clone(),
                "split": serde_json::to_value(&split).unwrap_or(Value::Null),
            }),
        );
        Ok(split)
    })();
    match result {
        Ok(split) => json::success_response(split, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_splits(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<SplitBill>> = (|| {
        let pagination = params.normalized();
        let mut items: Vec<SplitBill> = splits_store()
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|split| {
                matches_search(
                    &format!(
                        "{} {} {}",
                        split.customer_id,
                        split.order_id.clone().unwrap_or_default(),
                        split.invoice_id.clone().unwrap_or_default()
                    ),
                    &params,
                ) && params
                    .filters
                    .get("order_id")
                    .is_none_or(|value| split.order_id.as_deref() == Some(value.as_str()))
                    && params.filters.get("invoice_id").is_none_or(|value| {
                        split.invoice_id.as_deref() == Some(value.as_str())
                    })
                    && params.filters.get("customer_id").is_none_or(|value| {
                        split.customer_id == *value
                    })
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|value| split.status == *value)
                    && params.filters.get("provider").is_none_or(|value| {
                        split.parts.iter().any(|part| part.provider == *value)
                    })
                    && params.filters.get("currency").is_none_or(|value| {
                        split.currency.eq_ignore_ascii_case(value)
                    })
            })
            .collect();
        let sort = params.allowed_sort(&["created_at", "total_minor"], "created_at");
        match sort.as_str() {
            "total_minor" => items.sort_by_key(|split| split.total_minor),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if pagination.order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn get_split(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SplitBill> = find_split(id.into_inner());
    json::respond(result, debug)
}

async fn pay_split_part(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
    body: web::Json<PaySplitPartRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let (split_id, part_id) = path.into_inner();
        let request = body.into_inner();
        let split = find_split(split_id)?;
        if split.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Split is {} and cannot be paid",
                split.status
            )));
        }
        let part = split
            .parts
            .iter()
            .find(|part| part.id == part_id)
            .cloned()
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Split part \"{part_id}\" not found"))
            })?;
        if part.status == "paid" {
            return Err(UnifiedError::conflict("Split part is already paid"));
        }
        if part.status != "pending" {
            return Err(UnifiedError::unprocessable(format!(
                "Split part in status \"{}\" cannot be paid",
                part.status
            )));
        }
        if part.provider == "bank_transfer" {
            // No gateway charge: ask the bank-transfer module to open
            // a manual verification for this part. The module listens
            // to `bank_transfer.proof.requested` on the hook bus and
            // creates the verification; the administrator approval
            // then emits `payment.captured` with the split context,
            // which marks this part paid (see
            // `on_payment_captured_for_split`).
            state.emit(
                "bank_transfer.proof.requested",
                json!({
                    "split_id": split_id,
                    "part_id": part_id,
                    "order_id": split.order_id,
                    "invoice_id": split.invoice_id,
                    "customer_id": part.payer_id,
                    "amount_minor": part.amount_minor,
                    "currency": part.currency,
                    "provider": "bank_transfer",
                }),
            );
            return Ok(json!({
                "split_id": split_id,
                "part_id": part_id,
                "status": "pending",
                "provider": "bank_transfer",
                "next_step": "Transfer the amount to a configured bank account, then upload the payment proof with POST /api/v1/bank-transfer/verifications/{id}/proof. Find the verification with GET /api/v1/bank-transfer/verifications?split_id=<split_id>.",
            }));
        }
        let connection_id = request.connection_id.or(part.connection_id).ok_or_else(|| {
            UnifiedError::bad_request(
                "connection_id is required to pay this split part (pass it in the request or store it on the part)",
            )
        })?;
        let connection = find_connection(connection_id)?;
        let customer_id = request
            .customer_id
            .clone()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| part.payer_id.clone());
        let key = idempotency_key(request.idempotency_key.clone());
        if let Some(existing) = find_by_idempotency(connection.id, &key) {
            return Ok(json!({
                "split_id": split_id,
                "part_id": part_id,
                "transaction": existing,
                "status": "already_recorded",
            }));
        }
        let token = resolve_token(
            request.payment_method_token.as_deref(),
            request.payment_method_id,
            &customer_id,
        )?;
        // Batch 18 (S4): validate the card before any provider
        // call. The bank-transfer early return above stays before
        // the gate — no card is involved there. Raw details on the
        // request run in `full` mode; a stored method id runs in
        // `method` mode; an opaque token alone skips the gate.
        request_card_validation(&state, &req, request.card.as_ref(), request.payment_method_id, None, &connection, &customer_id, split.order_id.as_deref(), None)?;
        let adapter = adapter_for(&connection)?;
        let charge = adapter
            .charge(&ChargeInput {
                amount_minor: part.amount_minor,
                currency: part.currency.clone(),
                customer_id: customer_id.clone(),
                order_id: split.order_id.clone(),
                reference_id: None,
                invoice_id: split.invoice_id.clone(),
                description: request.description.clone(),
                payment_method_token: token,
                idempotency_key: key.clone(),
                three_ds: request.three_ds,
                billing: None,
            })
            .await?;
        let mut transaction = new_transaction(
            connection.id,
            connection.kind.as_str(),
            "split_part_charge",
            &customer_id,
            split.order_id.clone(),
            split.invoice_id.clone(),
            part.amount_minor,
            &part.currency,
            charge.status,
            charge.provider_transaction_id.clone(),
            key,
        );
        transaction.fee_minor = charge.fee_minor;
        let transaction = record_transaction(transaction);
        let payload = json!({
            "transaction_id": transaction.id,
            "provider_transaction_id": charge.provider_transaction_id,
            "provider": connection.kind.as_str(),
            "connection_id": connection.id,
            "customer_id": customer_id,
            "order_id": split.order_id,
            "invoice_id": split.invoice_id,
            "amount_minor": part.amount_minor,
            "currency": part.currency,
            "fee_minor": charge.fee_minor,
            "status": transaction.status,
            "split_id": split_id,
            "part_id": part_id,
        });
        match charge.status {
            PaymentStatus::Captured => state.emit(PAYMENT_CAPTURED, payload),
            PaymentStatus::Failed => {
                state.emit(PAYMENT_FAILED, payload);
                if let Some(mut entry) = splits_store().get_mut(&split_id) {
                    if let Some(failed) =
                        entry.parts.iter_mut().find(|part| part.id == part_id)
                    {
                        failed.status = "failed".to_string();
                    }
                    entry.updated_at = Utc::now();
                }
                notify(
                    &state,
                    &customer_id,
                    "payment_failed",
                    "Split payment failed",
                    &format!(
                        "Part {} of split {split_id} could not be paid.",
                        part.index + 1
                    ),
                );
                return Err(UnifiedError::bad_request(format!(
                    "Provider \"{}\" declined the payment for split part {part_id}",
                    connection.kind.as_str()
                ))
                .with_metadata(
                    "provider",
                    Value::String(connection.kind.as_str().to_string()),
                )
                .with_metadata(
                    "transaction_id",
                    Value::String(transaction.id.to_string()),
                ));
            }
            _ => {}
        }
        // The `payment.captured` listener marks the part paid
        // synchronously, so re-reading the split shows the new state.
        let updated = find_split(split_id)?;
        Ok(json!({ "split": updated, "transaction": transaction }))
    })()
    .await;
    json::respond(result, debug)
}

async fn cancel_split(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    path: web::Path<Uuid>,
    body: Option<web::Json<CancelSplitRequest>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SplitBill> = (|| {
        let id = path.into_inner();
        let reason = body.and_then(|body| body.into_inner().reason);
        let mut split = find_split(id)?;
        if split.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Split is {} and cannot be cancelled",
                split.status
            )));
        }
        if split.parts.iter().any(|part| part.status == "paid") {
            return Err(UnifiedError::conflict(
                "Cannot cancel a split with paid parts",
            ));
        }
        split.status = "cancelled".to_string();
        for part in split.parts.iter_mut() {
            if part.status == "pending" || part.status == "failed" {
                part.status = "cancelled".to_string();
            }
        }
        recompute_split_totals(&mut split);
        splits_store().insert(id, split.clone());
        state.emit(
            PAYMENT_SPLIT_CANCELLED,
            json!({
                "split_id": id,
                "order_id": split.order_id.clone(),
                "invoice_id": split.invoice_id.clone(),
                "customer_id": split.customer_id.clone(),
                "reason": reason,
            }),
        );
        Ok(split)
    })();
    json::respond(result, debug)
}

async fn generate_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    body: web::Json<GenerateScheduleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let input = body.into_inner();
        if input.total_minor <= 0 {
            return Err(UnifiedError::bad_request(
                "Schedule total must be greater than zero (integer minor units)",
            ));
        }
        if input.total_minor > MAX_AMOUNT_MINOR {
            return Err(UnifiedError::bad_request(
                "Schedule total exceeds the maximum accepted for one charge",
            ));
        }
        let currency = validate_currency(&input.currency)?;
        let customer_id = input.customer_id.trim().to_string();
        if customer_id.is_empty() {
            return Err(UnifiedError::bad_request("customer_id is required"));
        }
        if input.parts_count < 2 || input.parts_count > 60 {
            return Err(UnifiedError::unprocessable(
                "parts_count must be between 2 and 60",
            ));
        }
        let interval_days = input.interval_days.unwrap_or(30);
        if interval_days == 0 || interval_days > 365 {
            return Err(UnifiedError::unprocessable(
                "interval_days must be between 1 and 365",
            ));
        }
        let count = input.parts_count as usize;
        let amounts: Vec<i64> = match &input.amounts {
            Some(custom) => {
                if custom.len() != count {
                    return Err(UnifiedError::unprocessable(format!(
                        "Custom amounts has {} entries but parts_count is {count}",
                        custom.len()
                    )));
                }
                if custom.iter().any(|amount| *amount <= 0) {
                    return Err(UnifiedError::bad_request(
                        "Every custom amount must be greater than zero",
                    ));
                }
                let sum: i64 = custom.iter().sum();
                if sum != input.total_minor {
                    return Err(UnifiedError::unprocessable(format!(
                        "Custom amounts sum to {sum} {currency} but the total is {} {currency}",
                        input.total_minor
                    ))
                    .with_metadata("total_minor", Value::from(input.total_minor))
                    .with_metadata("parts_sum_minor", Value::from(sum))
                    .with_metadata(
                        "difference_minor",
                        Value::from(input.total_minor - sum),
                    ));
                }
                custom.clone()
            }
            None => {
                // Equal parts in integer minor units: the remainder
                // (total % count) goes one minor unit at a time to the
                // earliest parts, so the parts always sum to the total.
                let base = input.total_minor / input.parts_count as i64;
                let remainder = (input.total_minor % input.parts_count as i64) as usize;
                (0..count)
                    .map(|index| if index < remainder { base + 1 } else { base })
                    .collect()
            }
        };
        let payer_id = input
            .payer_id
            .clone()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| customer_id.clone());
        let provider = input
            .provider
            .clone()
            .map(|value| value.trim().to_lowercase())
            .filter(|value| !value.is_empty());
        if let Some(provider) = &provider {
            PaymentProviderKind::from_str(provider).ok_or_else(|| {
                UnifiedError::bad_request(format!("Unknown provider \"{provider}\""))
            })?;
        }
        let parts: Vec<Value> = amounts
            .iter()
            .enumerate()
            .map(|(index, amount)| {
                let due = input.first_due_date
                    + chrono::Duration::days(index as i64 * interval_days as i64);
                json!({
                    "index": index,
                    "amount_minor": amount,
                    "currency": currency.clone(),
                    "provider": provider.clone(),
                    "payer_id": payer_id.clone(),
                    "due_date": due.to_rfc3339(),
                })
            })
            .collect();
        let schedule = json!({
            "total_minor": input.total_minor,
            "currency": currency.clone(),
            "parts_count": input.parts_count,
            "interval_days": interval_days,
            "parts": parts,
        });
        if input.create {
            let provider = provider.ok_or_else(|| {
                UnifiedError::bad_request("provider is required when create is true")
            })?;
            let create = CreateSplitRequest {
                order_id: input.order_id.clone(),
                invoice_id: input.invoice_id.clone(),
                customer_id: customer_id.clone(),
                total_minor: input.total_minor,
                currency: currency.clone(),
                parts: amounts
                    .iter()
                    .enumerate()
                    .map(|(index, amount)| SplitPartInput {
                        amount_minor: *amount,
                        provider: Some(provider.clone()),
                        connection_id: input.connection_id,
                        payer_id: Some(payer_id.clone()),
                        due_date: Some(
                            input.first_due_date
                                + chrono::Duration::days(
                                    index as i64 * interval_days as i64,
                                ),
                        ),
                    })
                    .collect(),
            };
            let split = build_split_bill(create)?;
            splits_store().insert(split.id, split.clone());
            state.emit(
                PAYMENT_SPLIT_CREATED,
                json!({
                    "split_id": split.id,
                    "order_id": split.order_id.clone(),
                    "invoice_id": split.invoice_id.clone(),
                    "customer_id": split.customer_id.clone(),
                    "total_minor": split.total_minor,
                    "currency": split.currency.clone(),
                    "split": serde_json::to_value(&split).unwrap_or(Value::Null),
                }),
            );
            return Ok(json!({ "schedule": schedule, "split": split }));
        }
        Ok(json!({ "schedule": schedule }))
    })();
    json::respond(result, debug)
}


// ---------------------------------------------------------------------------
// Hook handlers (synchronous, like every other module's listeners).
// ---------------------------------------------------------------------------

/// A capture completed — through a gateway charge, an intent capture,
/// or a bank-transfer administrator approval. When the payload
/// carries `split_id` and `part_id`, mark that split part paid (the
/// single place where parts move to `paid`, so gateway and manual
/// payments share one code path), emit `payment.split.part.paid`,
/// and emit `payment.split.completed` when the full total is covered.
/// Captures without split context are ignored. The transition is
/// idempotent: an already-paid part is left untouched.
fn on_payment_captured_for_split(state: Arc<PaymentsState>, ctx: &mut HookContext) {
    let split_id = ctx
        .get("split_id")
        .and_then(|value| value.as_str())
        .and_then(|value| Uuid::parse_str(value).ok());
    let part_id = ctx
        .get("part_id")
        .and_then(|value| value.as_str())
        .and_then(|value| Uuid::parse_str(value).ok());
    let (Some(split_id), Some(part_id)) = (split_id, part_id) else {
        return;
    };
    let transaction_id = ctx
        .get("transaction_id")
        .and_then(|value| value.as_str())
        .and_then(|value| Uuid::parse_str(value).ok());
    let Some(mut split) = splits_store()
        .get(&split_id)
        .map(|entry| entry.value().clone())
    else {
        return;
    };
    if split.status != "active" {
        return;
    }
    let Some(part) = split.parts.iter_mut().find(|part| part.id == part_id) else {
        return;
    };
    if part.status != "pending" {
        return;
    }
    part.status = "paid".to_string();
    part.paid_transaction_id = transaction_id;
    part.paid_at = Some(Utc::now());
    let part_amount = part.amount_minor;
    let part_provider = part.provider.clone();
    let payer_id = part.payer_id.clone();
    recompute_split_totals(&mut split);
    let completed = split.remaining_minor == 0;
    if completed {
        split.status = "completed".to_string();
        split.updated_at = Utc::now();
    }
    let split_value = serde_json::to_value(&split).unwrap_or(Value::Null);
    splits_store().insert(split_id, split.clone());
    state.emit(
        PAYMENT_SPLIT_PART_PAID,
        json!({
            "split_id": split_id,
            "part_id": part_id,
            "order_id": split.order_id.clone(),
            "invoice_id": split.invoice_id.clone(),
            "customer_id": split.customer_id.clone(),
            "payer_id": payer_id,
            "provider": part_provider,
            "amount_minor": part_amount,
            "currency": split.currency.clone(),
            "transaction_id": transaction_id,
            "split": split_value.clone(),
        }),
    );
    if completed {
        state.emit(
            PAYMENT_SPLIT_COMPLETED,
            json!({
                "split_id": split_id,
                "order_id": split.order_id.clone(),
                "invoice_id": split.invoice_id.clone(),
                "customer_id": split.customer_id.clone(),
                "total_minor": split.total_minor,
                "currency": split.currency.clone(),
                "split": split_value,
            }),
        );
    }
}

/// The store started a checkout through
/// `POST /api/v1/store/orders/{id}/pay` and emitted
/// `payment.initiated` with `{ "order", "provider", "amount" }`.
/// Hook handlers are synchronous, so the gateway cannot charge here;
/// the checkout is recorded so a later confirm / pay call can answer
/// it, and the result tells the store the gateway accepted the
/// handoff.
fn on_payment_initiated(state: Arc<PaymentsState>, ctx: &mut HookContext) {
    let _ = state;
    let order_id = ctx
        .get("order")
        .and_then(|order| order.get("id"))
        .and_then(|id| id.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order_id")
                .and_then(|id| id.as_str())
                .map(str::to_string)
        });
    let Some(order_id) = order_id else {
        ctx.add_result(json!({
            "gateway": "payments",
            "status": "rejected",
            "error": "payment.initiated carries no order id",
        }));
        return;
    };
    pending_checkouts_store().insert(order_id.clone(), ctx.data.clone());
    ctx.add_result(json!({
        "gateway": "payments",
        "status": "accepted",
        "order_id": order_id,
        "note": "Checkout recorded. Confirm with POST /api/v1/payments/orders/{order_id}/pay (charge) or POST /api/v1/payments/intents/{id}/confirm (intent).",
    }));
}

/// The store refunded an order (`store.order.refunded`) after marking
/// it refunded. The gateway-first refund path emits
/// `payment.refunded` itself; this listener re-emits it for the
/// store-first path, so downstream listeners that only know
/// `payment.refunded` (billing, licensing) still fire. Licensing
/// revocation is idempotent (an already-revoked license stays
/// revoked), so a double emit is safe.
fn on_store_order_refunded(state: Arc<PaymentsState>, ctx: &mut HookContext) {
    let order_id = ctx
        .get("order_id")
        .and_then(|id| id.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order")
                .and_then(|order| order.get("id"))
                .and_then(|id| id.as_str())
                .map(str::to_string)
        });
    let Some(order_id) = order_id else {
        return;
    };
    // A store-refunded order is a completed refund by construction
    // (the store only marks it refunded on the completion path), so
    // the re-emitted event carries an explicit completed status and
    // the refund identifiers the billing listener needs for its
    // status gate and its redelivery dedupe.
    state.emit(
        PAYMENT_REFUNDED,
        json!({
            "order_id": order_id,
            "license_ids": ctx.get("license_ids").cloned().unwrap_or(Value::Null),
            "reason": ctx.get("reason").cloned().unwrap_or(Value::Null),
            "transaction_id": ctx.get("transaction_id").cloned().unwrap_or(Value::Null),
            "provider_refund_id": ctx.get("provider_refund_id").cloned().unwrap_or(Value::Null),
            "amount_minor": ctx.get("amount_minor").cloned().unwrap_or(Value::Null),
            "currency": ctx.get("currency").cloned().unwrap_or(Value::Null),
            "status": "refunded",
            "fully_refunded": true,
            "source": "store.order.refunded",
        }),
    );
}

/// A renewal was requested for a subscription
/// (`payment.subscription.renew_requested`, emitted by an operator,
/// a cron, or the scheduler once it knows the kind).
///
/// Two branches, and they must not be confused: an
/// operator-initiated request only schedules the charge by asking
/// the scheduler to run a `payment.subscription.renew` job at
/// `renew_at`. A scheduler-dispatched request carries
/// `via_scheduler: true` and is the terminal step: it runs the
/// charge instead of re-enqueuing. Without that distinction each
/// dispatch spawns one new queue item forever and the renewal never
/// runs, because the handler would answer its own dispatched event
/// by enqueuing the same job again.
fn on_subscription_renew_requested(state: Arc<PaymentsState>, ctx: &mut HookContext) {
    let Some(subscription_id) = ctx
        .get("subscription_id")
        .and_then(|id| id.as_str())
        .and_then(|id| Uuid::parse_str(id).ok())
    else {
        ctx.add_result(json!({
            "gateway": "payments",
            "status": "rejected",
            "error": "payment.subscription.renew_requested needs subscription_id",
        }));
        return;
    };
    // Scheduler-dispatched request: run the charge, never re-enqueue.
    // The charge is asynchronous and this handler is synchronous, so
    // it is spawned (the outcome is reported through the renewal
    // events: `payment.subscription.renewed` on success,
    // `payment.failed` on failure). The scheduler's 30-second
    // per-subscription dedupe stays in place as a second guard
    // against duplicate dispatches.
    if ctx
        .get("via_scheduler")
        .and_then(|value| value.as_bool())
        == Some(true)
    {
        let worker_state = state.clone();
        spawn_renewal_worker(async move {
            let _ = renew_one(&worker_state, subscription_id).await;
        });
        ctx.add_result(json!({
            "gateway": "payments",
            "subscription_id": subscription_id,
            "via_scheduler": true,
            "queued": false,
            "renewal": "started",
        }));
        return;
    }
    let renew_at = ctx
        .get("renew_at")
        .and_then(|value| value.as_str())
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);
    let queued = enqueue_renewal(&state, subscription_id, renew_at);
    ctx.add_result(json!({
        "gateway": "payments",
        "subscription_id": subscription_id,
        "queued": queued,
    }));
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct PaymentsModule {
    state: Arc<PaymentsState>,
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
            RouteInfo { method: "GET", path: "/api/v1/payments/providers", owner: "payments", description: "Provider catalog with capability matrix, currencies, and settings fields" },
            RouteInfo { method: "GET", path: "/api/v1/payments/providers/{kind}", owner: "payments", description: "One provider's catalog entry" },
            RouteInfo { method: "GET", path: "/api/v1/payments/providers/{kind}/settings", owner: "payments", description: "Settings-page field definitions for one provider" },
            RouteInfo { method: "GET", path: "/api/v1/payments/cards/settings", owner: "payments", description: "Card validation contract: capabilities, validation event and reason codes, and pointers to the canonical card and reference-id settings surfaces (admin-only, cards:read)" },
            RouteInfo { method: "GET", path: "/api/v1/payments/connections", owner: "payments", description: "List provider connections (credentials masked)" },
            RouteInfo { method: "POST", path: "/api/v1/payments/connections", owner: "payments", description: "Create a provider connection" },
            RouteInfo { method: "GET", path: "/api/v1/payments/connections/{id}", owner: "payments", description: "Get one connection (credentials masked)" },
            RouteInfo { method: "PUT", path: "/api/v1/payments/connections/{id}", owner: "payments", description: "Update a connection (credentials merged, never returned)" },
            RouteInfo { method: "DELETE", path: "/api/v1/payments/connections/{id}", owner: "payments", description: "Delete a connection" },
            RouteInfo { method: "POST", path: "/api/v1/payments/connections/{id}/test", owner: "payments", description: "Test a connection against the real provider API" },
            RouteInfo { method: "POST", path: "/api/v1/payments/connections/{id}/mode", owner: "payments", description: "Switch a connection between sandbox and live mode" },
            RouteInfo { method: "POST", path: "/api/v1/payments/intents", owner: "payments", description: "Create a payment intent" },
            RouteInfo { method: "GET", path: "/api/v1/payments/intents/{id}", owner: "payments", description: "Get one payment intent" },
            RouteInfo { method: "POST", path: "/api/v1/payments/intents/{id}/confirm", owner: "payments", description: "Confirm an intent (authorize or charge)" },
            RouteInfo { method: "POST", path: "/api/v1/payments/intents/{id}/capture", owner: "payments", description: "Capture an authorized intent" },
            RouteInfo { method: "POST", path: "/api/v1/payments/intents/{id}/cancel", owner: "payments", description: "Cancel an intent (voids an authorization at the provider)" },
            RouteInfo { method: "POST", path: "/api/v1/payments/charges", owner: "payments", description: "Charge a tokenized payment method directly" },
            RouteInfo { method: "GET", path: "/api/v1/payments/transactions", owner: "payments", description: "List transactions with provider/status/currency/customer/date filters" },
            RouteInfo { method: "GET", path: "/api/v1/payments/transactions/{id}", owner: "payments", description: "Get one transaction" },
            RouteInfo { method: "POST", path: "/api/v1/payments/transactions/{id}/refund", owner: "payments", description: "Refund a captured charge (gateway first, then the refund event)" },
            RouteInfo { method: "POST", path: "/api/v1/payments/transactions/{id}/complete-refund", owner: "payments", description: "Complete a pending refund (refunded or failed) and emit the refund event" },
            RouteInfo { method: "POST", path: "/api/v1/payments/transactions/{id}/void", owner: "payments", description: "Void an authorization" },
            RouteInfo { method: "POST", path: "/api/v1/payments/payouts", owner: "payments", description: "Pay out through a provider that supports payouts" },
            RouteInfo { method: "GET", path: "/api/v1/payments/methods", owner: "payments", description: "List tokenized payment methods per customer" },
            RouteInfo { method: "POST", path: "/api/v1/payments/methods", owner: "payments", description: "Store a tokenized payment method (never a PAN)" },
            RouteInfo { method: "GET", path: "/api/v1/payments/methods/{id}", owner: "payments", description: "Get one payment method" },
            RouteInfo { method: "DELETE", path: "/api/v1/payments/methods/{id}", owner: "payments", description: "Delete a payment method" },
            RouteInfo { method: "POST", path: "/api/v1/payments/methods/{id}/set-default", owner: "payments", description: "Mark a payment method as the customer default" },
            RouteInfo { method: "GET", path: "/api/v1/payments/subscriptions", owner: "payments", description: "List gateway subscriptions" },
            RouteInfo { method: "POST", path: "/api/v1/payments/subscriptions", owner: "payments", description: "Create a gateway subscription" },
            RouteInfo { method: "POST", path: "/api/v1/payments/subscriptions/renew-due", owner: "payments", description: "Renew every subscription whose renewal time has arrived" },
            RouteInfo { method: "GET", path: "/api/v1/payments/subscriptions/{id}", owner: "payments", description: "Get one subscription" },
            RouteInfo { method: "POST", path: "/api/v1/payments/subscriptions/{id}/cancel", owner: "payments", description: "Cancel a subscription" },
            RouteInfo { method: "POST", path: "/api/v1/payments/subscriptions/{id}/renew", owner: "payments", description: "Renew one subscription now" },
            RouteInfo { method: "POST", path: "/api/v1/payments/orders/{order_id}/pay", owner: "payments", description: "Pay for a store order through the gateway" },
            RouteInfo { method: "GET", path: "/api/v1/payments/splits", owner: "payments", description: "List split bills with pagination and filters" },
            RouteInfo { method: "POST", path: "/api/v1/payments/splits", owner: "payments", description: "Create a split bill; parts must sum exactly to the total (422 with per-part math otherwise)" },
            RouteInfo { method: "POST", path: "/api/v1/payments/splits/schedule", owner: "payments", description: "Generate an instalment schedule (equal parts or custom amounts, due dates), optionally creating the split" },
            RouteInfo { method: "GET", path: "/api/v1/payments/splits/{id}", owner: "payments", description: "Get one split bill with its parts" },
            RouteInfo { method: "POST", path: "/api/v1/payments/splits/{id}/parts/{part_id}/pay", owner: "payments", description: "Pay one split part through any provider, including bank transfer (manual verification)" },
            RouteInfo { method: "POST", path: "/api/v1/payments/splits/{id}/cancel", owner: "payments", description: "Cancel a split bill (only while no part is paid)" },
            RouteInfo { method: "POST", path: "/api/v1/payments/webhooks/{provider}", owner: "payments", description: "Receive a provider webhook (signature verified first)" },
            RouteInfo { method: "GET", path: "/api/v1/payments/webhooks/events", owner: "payments", description: "List processed webhook event ids" },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(PaymentsModule::new()));
}
