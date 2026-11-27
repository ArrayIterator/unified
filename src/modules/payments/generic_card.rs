//! payments - generic card.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use reqwest::header::{HeaderMap};
use reqwest::{Method, StatusCode};
use serde_json::{json};
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperswitch::*, twocheckout::*, mpgs::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



// Generic card processing.
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
    pub(crate) async fn card_charge(
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

    pub(crate) async fn card_capture(
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

    pub(crate) async fn card_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
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

    pub(crate) async fn card_void(&self, provider_transaction_id: &str) -> UnifiedResult<()> {
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
    pub(crate) async fn card_test(&self) -> UnifiedResult<String> {
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
    pub(crate) fn card_webhook_verify(
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
