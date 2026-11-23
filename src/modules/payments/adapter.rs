//! payments - adapter.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use actix_web::{HttpRequest};
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::{StatusCode};
use serde_json::{json, Value};
use sha2::{Digest, Sha256, Sha512};
use std::time::Duration;
use url::Url;
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperpay::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



#[async_trait]
// ---------------------------------------------------------------------------
// Common adapter interface.
// ---------------------------------------------------------------------------
/// The interface every payment adapter implements. Only `charge` is
/// required in the sense that every kind supports it; every other
/// operation has a default implementation that fails with a clear
/// "not supported" error naming the missing capability, so a
/// charge-only adapter only implements `kind`, `charge`,
/// `test_connection`, and `webhook_verify`.
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


pub(crate) fn unsupported(kind: PaymentProviderKind, action: &str, capability: &str) -> UnifiedError {
    UnifiedError::bad_request(format!(
        "Provider \"{}\" does not support {action} (capability {capability} is false)",
        kind.as_str()
    ))
}



// ---------------------------------------------------------------------------
// Shared HTTP helpers.
// ---------------------------------------------------------------------------
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}


pub(crate) fn sha512_hex(data: &[u8]) -> String {
    hex_encode(&Sha512::digest(data))
}


pub(crate) type HmacSha256 = Hmac<Sha256>;


pub(crate) fn hmac_sha256_hex(key: &[u8], message: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key length is always valid");
    mac.update(message);
    hex_encode(&mac.finalize().into_bytes())
}


/// Constant-time-ish equality for signatures (length check first, then
/// a full scan so the timing does not leak the mismatch position).
pub(crate) fn digests_equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}


pub(crate) fn build_client() -> UnifiedResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS))
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))
}


/// Refuse a URL whose host is not on the provider's allow-list. Called
/// before every request to a fixed-host provider.
pub(crate) fn check_allowlist(url: &Url, kind: PaymentProviderKind) -> UnifiedResult<()> {
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
pub(crate) fn parse_retry_after(response: &reqwest::Response) -> Option<Duration> {
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
pub(crate) fn map_provider_error(
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
pub(crate) async fn send_json(
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
pub(crate) async fn call_raw(
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
pub(crate) fn json_str<'a>(body: &'a Value, keys: &[&str]) -> Option<&'a str> {
    for key in keys {
        if let Some(value) = body.get(key).and_then(|value| value.as_str()) {
            return Some(value);
        }
    }
    None
}


/// Read the first present integer field from a JSON body.
pub(crate) fn json_i64(body: &Value, keys: &[&str]) -> Option<i64> {
    for key in keys {
        if let Some(value) = body.get(key).and_then(|value| value.as_i64()) {
            return Some(value);
        }
    }
    None
}


/// Read a fee from the common fee field names, when the provider
/// reports one. Fees are never estimated: no field means no fee.
pub(crate) fn fee_from(body: &Value) -> Option<i64> {
    json_i64(
        body,
        &["fee", "fee_amount", "processing_fee", "application_fee_amount"],
    )
}


/// Currencies with no minor unit (amounts are already whole units).
pub(crate) fn is_zero_decimal(currency: &str) -> bool {
    matches!(currency, "JPY" | "KRW" | "VND" | "CLP")
}


/// Convert a major-unit amount (a store order total) to integer
/// minor units, mirroring the store's conversion. The f64 input
/// arrives already parsed from a JSON number (a pending-checkout
/// total); it is never multiplied as a float, so 10.05 cannot
/// become 1004 through binary-float error. The value is formatted
/// to the currency's decimal places first, then converted from the
/// decimal string with integer arithmetic.
pub(crate) fn major_to_minor(amount_major: f64, currency: &str) -> i64 {
    if !amount_major.is_finite() {
        return 0;
    }
    let text = if is_zero_decimal(currency) {
        format!("{amount_major:.0}")
    } else {
        format!("{amount_major:.2}")
    };
    // The formatted text of a finite float always parses.
    decimal_str_to_minor(&text, currency).unwrap_or(0)
}


/// Convert a major-unit decimal string (for example `"10.50"` or
/// `"10000"`) to integer minor units with integer arithmetic only —
/// no float is involved at any point. Rounds half-up on the first
/// discarded fraction digit. Provider-facing code must use this
/// (or `major_amount` / `major_decimal_value`) instead of parsing
/// amounts as f64.
pub(crate) fn decimal_str_to_minor(text: &str, currency: &str) -> UnifiedResult<i64> {
    let text = text.trim();
    let invalid = || UnifiedError::bad_request(format!("Amount \"{text}\" is not a decimal number"));
    if text.is_empty() {
        return Err(invalid());
    }
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let digits = digits.strip_prefix('+').unwrap_or(digits);
    let (whole, frac) = match digits.split_once('.') {
        Some((whole, frac)) => (whole, frac),
        None => (digits, ""),
    };
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !frac.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid());
    }
    let whole: i128 = whole.parse().map_err(|_| invalid())?;
    if is_zero_decimal(currency) {
        // Whole units; round on the first fraction digit.
        let mut value = whole;
        if frac.as_bytes().first().is_some_and(|digit| *digit >= b'5') {
            value += 1;
        }
        let value = if negative { -value } else { value };
        return i64::try_from(value).map_err(|_| invalid());
    }
    let padded = format!("{frac:0<2}");
    let first_two: i128 = padded[..2].parse().map_err(|_| invalid())?;
    let mut minor = whole
        .checked_mul(100)
        .and_then(|value| value.checked_add(first_two))
        .ok_or_else(invalid)?;
    if frac.as_bytes().get(2).is_some_and(|digit| *digit >= b'5') {
        minor += 1;
    }
    let minor = if negative { -minor } else { minor };
    i64::try_from(minor).map_err(|_| invalid())
}


/// A major-unit amount as an exact JSON number (for example `10.5`
/// for 1050 minor units of USD), built from the decimal string so
/// no float ever holds the amount. Replaces the old
/// `major_amount(...).parse::<f64>().unwrap_or(0.0)` shape, which
/// silently sent 0.0 on a malformed string.
pub(crate) fn major_decimal_value(amount_minor: i64, currency: &str) -> UnifiedResult<Value> {
    let text = major_amount(amount_minor, currency);
    serde_json::from_str::<Value>(&text).map_err(|_| {
        UnifiedError::internal(format!(
            "Cannot represent amount {amount_minor} {currency} as a decimal number"
        ))
    })
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
pub(crate) fn check_pending_checkout_amount(
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
    let order_currency = order
        .and_then(|order| order.get("currency"))
        .and_then(|value| value.as_str());
    // The order total arrives as integer minor units (legacy
    // checkouts stored major-unit floats, still accepted). An
    // integer total is already minor units; a float total goes
    // through the currency-aware conversion.
    let total_minor = order
        .and_then(|order| order.get("total"))
        .and_then(|value| {
            value.as_i64().or_else(|| {
                value
                    .as_f64()
                    .filter(|major| major.is_finite())
                    .map(|major| {
                        major_to_minor(major, order_currency.unwrap_or("USD"))
                    })
            })
        });
    let (Some(total_minor), Some(order_currency)) = (total_minor, order_currency) else {
        return Err(UnifiedError::unprocessable(
            "Checkout record for this order has no verifiable total",
        ));
    };
    if !currency.eq_ignore_ascii_case(order_currency) || amount_minor != total_minor {
        return Err(UnifiedError::unprocessable(
            "Amount does not match the order total for this checkout",
        ));
    }
    Ok(())
}


/// Caller identities from the server-verified caller. These are NOT
/// authentication, but they are the only caller identity the payments
/// module can see, so mutating endpoints use them as a
/// defence-in-depth ownership check: a caller may only touch records
/// that belong to the verified user id. A caller with no verified
/// identity matches only `guest`-owned records.
pub(crate) fn caller_identities(req: &HttpRequest) -> Vec<String> {
    let verified = unified_core::caller::caller_from(req);
    if verified.is_authenticated() {
        vec![verified.user_id]
    } else {
        Vec::new()
    }
}


pub(crate) fn caller_owns(req: &HttpRequest, owner: &str) -> bool {
    let identities = caller_identities(req);
    if identities.is_empty() {
        return owner == "guest";
    }
    identities.iter().any(|identity| identity == owner)
}


/// Fail closed when the caller does not own the record they are
/// mutating. `what` names the record in the error message.
pub(crate) fn require_customer(req: &HttpRequest, owner: &str, what: &str) -> UnifiedResult<()> {
    if caller_owns(req, owner) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Caller does not own this {what}"
        )))
    }
}


/// Fail closed unless the caller owns at least one of the given
/// owners (for example a split part's payer or the split's
/// customer). Used where a record has more than one legitimate
/// owner and a single-owner check would reject a valid caller.
pub(crate) fn require_customer_any(
    req: &HttpRequest,
    owners: &[&str],
    what: &str,
) -> UnifiedResult<()> {
    if owners.iter().any(|owner| caller_owns(req, owner)) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Caller does not own this {what}"
        )))
    }
}


/// Capability an operator must hold for payment administration:
/// managing gateway connections and moving money out through
/// payouts. Checked through the `role.check` hook exactly like
/// the tenants and printing modules do, and fails closed on
/// silence (no roles module answering means denied). The role
/// comes from the `x-role` header — the same trusted-header
/// caveat as the rest of the codebase applies until the auth
/// extractor lands.
pub(crate) const PAYMENTS_ADMIN_PERMISSION: &str = "payments:admin";


/// The caller's `x-user-id` identity. Missing means the caller is
/// unauthenticated for the endpoints that require an identity.
pub(crate) fn caller_user_id(req: &HttpRequest) -> UnifiedResult<String> {
    Ok(unified_core::caller::require_caller(req)?.user_id)
}


/// True when the caller holds the payments admin capability.
pub(crate) fn payments_admin_allowed_on_bus(bus: &HookBus, req: &HttpRequest) -> bool {
    let Some(role) = unified_core::caller::caller_from(req)
        .roles
        .into_iter()
        .next()
    else {
        return false;
    };
    let outcome = bus.emit(
        "role.check",
        json!({ "role": role, "permission": PAYMENTS_ADMIN_PERMISSION, "allowed": false }),
    );
    outcome
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}


/// Fail unless the caller is an authenticated operator holding the
/// payments admin capability. Returns the operator's `x-user-id`
/// so the caller can record the payout or connection change
/// against an operator identity. The `_on_bus` variant takes the
/// hook bus directly so tests can drive it without a full
/// `AppContext`; handlers use `require_payments_admin`.
pub(crate) fn require_payments_admin_on_bus(
    bus: &HookBus,
    req: &HttpRequest,
) -> UnifiedResult<String> {
    let actor = caller_user_id(req)?;
    if payments_admin_allowed_on_bus(bus, req) {
        Ok(actor)
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role lacks required capability: {PAYMENTS_ADMIN_PERMISSION}"
        )))
    }
}


pub(crate) fn require_payments_admin(
    ctx: &AppContext,
    req: &HttpRequest,
) -> UnifiedResult<String> {
    require_payments_admin_on_bus(&ctx.hooks, req)
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 5, group C).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before any identity, capability, or
/// ownership check.
///
/// Records in this module carry no `company_id` / `tenant_id`
/// field, so the guard is a fail-closed gate plus the module's
/// existing verified-caller ownership checks (`require_customer` /
/// `list_customer_scope` on `customer_id`): no per-record company
/// check is applied. Real per-tenant partitioning needs a
/// `tenant_id` on the records, a C11 follow-up.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}


/// Scope for a list endpoint: `None` when the caller is a payments
/// admin (sees every customer's records), otherwise the caller's
/// own `x-user-id`, which the list must force as a `customer_id`
/// filter. A caller with no identity at all is refused, so a list
/// can never silently return every customer's records.
pub(crate) fn list_customer_scope_on_bus(
    bus: &HookBus,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    if payments_admin_allowed_on_bus(bus, req) {
        Ok(None)
    } else {
        Ok(Some(caller_user_id(req)?))
    }
}


pub(crate) fn list_customer_scope(
    ctx: &AppContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    list_customer_scope_on_bus(&ctx.hooks, req)
}


/// Success status for a charge / pay / confirm outcome: 201 only
/// when money was actually captured; every other recorded outcome
/// — pending, requires_confirmation, authorized — is 202, so
/// callers can tell "paid" from "pending" by status code instead
/// of only by the body's `status` field. The response body still
/// carries the provider status.
pub(crate) fn charge_success_status(status: &str) -> u16 {
    if status == "captured" {
        201
    } else {
        202
    }
}


/// Format minor units as a major-unit decimal string for providers
/// that take amounts as decimals (`"10.50"`, or `"1050"` for
/// zero-decimal currencies).
pub(crate) fn major_amount(amount_minor: i64, currency: &str) -> String {
    if is_zero_decimal(currency) {
        return amount_minor.to_string();
    }
    format!("{}.{:02}", amount_minor / 100, (amount_minor % 100).abs())
}


/// Major units as a float. Legacy helper only: provider-facing
/// code must use `major_amount` (decimal string) or
/// `major_decimal_value` (exact JSON number) instead, so no float
/// ever carries money to a provider. Kept for the re-exported
/// adapter surface.
pub(crate) fn major_f64(amount_minor: i64, currency: &str) -> f64 {
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
pub(crate) fn major_i64(amount_minor: i64, currency: &str) -> i64 {
    if is_zero_decimal(currency) {
        amount_minor
    } else if amount_minor >= 0 {
        (amount_minor + 50) / 100
    } else {
        (amount_minor - 50) / 100
    }
}


/// Validate an amount and currency before any provider call.
pub(crate) fn validate_money(amount_minor: i64, currency: &str) -> UnifiedResult<String> {
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
pub(crate) fn validate_currency(currency: &str) -> UnifiedResult<String> {
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
pub(crate) fn looks_like_pan(value: &str) -> bool {
    let digits: String = value.chars().filter(|c| c.is_ascii_digit()).collect();
    let stripped: String = value
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == ' ' || *c == '-')
        .collect();
    stripped.len() == value.len() && (13..=19).contains(&digits.len()) && digits.len() == value.replace([' ', '-'], "").len()
}


/// Validate a payment method token: present, non-empty, and not a PAN.
pub(crate) fn validate_token(token: Option<&str>, provider: &str) -> UnifiedResult<String> {
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
pub(crate) fn stored_transaction(provider_transaction_id: &str) -> Option<PaymentTransaction> {
    transactions_store()
        .iter()
        .find(|entry| entry.value().provider_transaction_id == provider_transaction_id)
        .map(|entry| entry.value().clone())
}


pub(crate) fn stored_transaction_currency(provider_transaction_id: &str) -> UnifiedResult<String> {
    stored_transaction(provider_transaction_id)
        .map(|transaction| transaction.currency)
        .ok_or_else(|| {
            UnifiedError::not_found(format!(
                "No stored transaction for provider transaction id \"{provider_transaction_id}\"; capture needs the original currency"
            ))
        })
}


/// Map a provider-native status string onto the shared status.
pub(crate) fn map_status(kind: PaymentProviderKind, raw: &str) -> PaymentStatus {
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

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;
    use unified_core::hooks::{HookBus, HookContext};
    use actix_web::test::TestRequest;
    use std::sync::Arc;

    fn bus_with_role_check(allowed: bool) -> Arc<HookBus> {
        let bus = Arc::new(HookBus::new());
        bus.register(
            "role.check",
            "test",
            Arc::new(move |context: &mut HookContext| {
                context.set("allowed", Value::Bool(allowed));
            }),
        );
        bus
    }

    fn admin_request() -> HttpRequest {
        verified_request("op-1", "admin")
    }

    #[test]
    fn decimal_str_to_minor_converts_without_floats() {
        assert_eq!(decimal_str_to_minor("10.50", "USD").unwrap(), 1050);
        assert_eq!(decimal_str_to_minor("10.05", "USD").unwrap(), 1005);
        assert_eq!(decimal_str_to_minor("10", "USD").unwrap(), 1000);
        // Half-up rounding on the third fraction digit.
        assert_eq!(decimal_str_to_minor("10.055", "USD").unwrap(), 1006);
        assert_eq!(decimal_str_to_minor("10.054", "USD").unwrap(), 1005);
        // Zero-decimal currencies are already whole units.
        assert_eq!(decimal_str_to_minor("10000.00", "IDR").unwrap(), 10000);
        assert_eq!(decimal_str_to_minor("10000.6", "IDR").unwrap(), 10001);
        assert_eq!(decimal_str_to_minor("-2.50", "USD").unwrap(), -250);
        assert!(decimal_str_to_minor("abc", "USD").is_err());
        assert!(decimal_str_to_minor("", "USD").is_err());
        assert!(decimal_str_to_minor("10.5.2", "USD").is_err());
    }

    #[test]
    fn major_to_minor_no_longer_multiplies_a_float() {
        // 10.05 as f64 is 10.05000000000000071; a naive `* 100.0`
        // followed by truncation would give 1004. The decimal-string
        // conversion gives the exact 1005.
        assert_eq!(major_to_minor(10.05, "USD"), 1005);
        assert_eq!(major_to_minor(4.35, "USD"), 435);
        assert_eq!(major_to_minor(10000.0, "IDR"), 10000);
    }

    #[test]
    fn major_decimal_value_is_an_exact_json_number() {
        assert_eq!(
            major_decimal_value(1050, "USD").unwrap(),
            serde_json::json!(10.5)
        );
        assert_eq!(
            major_decimal_value(1, "USD").unwrap(),
            serde_json::json!(0.01)
        );
        // Never a silent 0.0 for a real amount.
        assert_ne!(
            major_decimal_value(1050, "USD").unwrap(),
            serde_json::json!(0.0)
        );
    }

    #[test]
    fn charge_success_status_only_captured_is_201() {
        assert_eq!(charge_success_status("captured"), 201);
        assert_eq!(charge_success_status("pending"), 202);
        assert_eq!(charge_success_status("authorized"), 202);
        assert_eq!(charge_success_status("requires_confirmation"), 202);
    }

    #[test]
    fn payments_admin_gate_fails_closed_without_a_role_answer() {
        let bus = Arc::new(HookBus::new());
        let req = admin_request();
        // No `role.check` handler registered: silence means denied.
        assert!(require_payments_admin_on_bus(&bus, &req).is_err());
        // A denying handler also refuses.
        let bus = bus_with_role_check(false);
        assert!(require_payments_admin_on_bus(&bus, &req).is_err());
        // An allowing handler admits the operator and returns the actor.
        let bus = bus_with_role_check(true);
        assert_eq!(
            require_payments_admin_on_bus(&bus, &req).unwrap(),
            "op-1"
        );
        // No identity at all is refused before any capability check.
        let anonymous = TestRequest::default().to_http_request();
        assert!(require_payments_admin_on_bus(&bus, &anonymous).is_err());
        // No role header is refused even with an allowing handler.
        let no_role = verified_request_without_roles("op-1");
        assert!(require_payments_admin_on_bus(&bus, &no_role).is_err());
    }

    #[test]
    fn list_scope_is_admin_unfiltered_or_the_callers_own_id() {
        let admin_bus = bus_with_role_check(true);
        assert_eq!(
            list_customer_scope_on_bus(&admin_bus, &admin_request()).unwrap(),
            None
        );
        let plain_bus = Arc::new(HookBus::new());
        let req = verified_request_without_roles("cust-7");
        assert_eq!(
            list_customer_scope_on_bus(&plain_bus, &req).unwrap(),
            Some("cust-7".to_string())
        );
        let anonymous = TestRequest::default().to_http_request();
        assert!(list_customer_scope_on_bus(&plain_bus, &anonymous).is_err());
    }

    #[test]
    fn require_customer_any_accepts_any_listed_owner() {
        let req = verified_request_without_roles("cust-7");
        assert!(require_customer_any(&req, &["cust-1", "cust-7"], "split part").is_ok());
        assert!(require_customer_any(&req, &["cust-1", "cust-2"], "split part").is_err());
    }
    #[test]
    fn hex_encode_matches_reference_for_every_byte_value() {
        // Exhaustive oracle: the lookup-table encoding must agree
        // with plain per-byte formatting for all 256 byte values.
        // (`format!` is the reference here, in test code only.)
        for byte in 0..=u8::MAX {
            assert_eq!(hex_encode(&[byte]), format!("{byte:02x}"));
        }
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
    }

}
