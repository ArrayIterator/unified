//! payments - hyperswitch.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use reqwest::header::{HeaderMap};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use uuid::Uuid;
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



// ---------------------------------------------------------------------------
// Hyperswitch (Juspay open-source payment switch).
// ---------------------------------------------------------------------------
impl ProviderAdapter {
    /// Hyperswitch charge / authorize. One unified API: POST
    /// /payments with the amount in minor units. `confirm` is true
    /// when a tokenized payment method is present, so the payment is
    /// confirmed immediately; without a token the payment is created
    /// unconfirmed and the shopper completes it with the returned
    /// client_secret (Hyperswitch SDK / hosted checkout), so this
    /// call returns `pending` with `requires_action`. `capture_method`
    /// is `manual` for authorize (the payment parks in
    /// `requires_capture`) and `automatic` for a charge. The caller
    /// reference is sent as `payment_id`, which Hyperswitch treats as
    /// the idempotency key for the create call.
    pub(crate) async fn hyperswitch_charge(
        &self,
        input: &ChargeInput,
        authorize_only: bool,
    ) -> UnifiedResult<ChargeResult> {
        let token = match &input.payment_method_token {
            Some(token) => Some(validate_token(Some(token), "hyperswitch")?),
            None => None,
        };
        let url = self.url("/payments")?;
        let mut body = json!({
            "payment_id": charge_reference(input),
            "amount": input.amount_minor,
            "currency": input.currency.to_uppercase(),
            "confirm": token.is_some(),
            "capture_method": if authorize_only { "manual" } else { "automatic" },
        });
        if let Some(token) = &token {
            body["payment_token"] = json!(token);
        }
        if let Some(profile_id) = self.credentials.profile_id.clone().filter(|v| !v.is_empty()) {
            body["profile_id"] = json!(profile_id);
        }
        if let Some(description) = &input.description {
            body["description"] = json!(description);
        }
        if input.three_ds {
            body["authentication_type"] = json!("three_ds");
        }
        let mut metadata = json!({ "customer_id": input.customer_id });
        if let Some(order_id) = &input.order_id {
            metadata["order_id"] = json!(order_id);
        }
        if let Some(invoice_id) = &input.invoice_id {
            metadata["invoice_id"] = json!(invoice_id);
        }
        body["metadata"] = metadata;
        let response = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .json(&body),
            "hyperswitch",
        )
        .await?;
        let id = json_str(&response, &["payment_id"])
            .ok_or_else(|| UnifiedError::internal("Hyperswitch response has no payment_id"))?
            .to_string();
        let raw_status = json_str(&response, &["status"]).unwrap_or("unknown");
        let status = map_status(PaymentProviderKind::Hyperswitch, raw_status);
        let redirect_url = response
            .get("next_action")
            .and_then(|action| action.get("redirect_to_url"))
            .and_then(|target| target.get("url"))
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let pending = status == PaymentStatus::Pending
            || status == PaymentStatus::RequiresConfirmation;
        Ok(ChargeResult {
            provider_transaction_id: id,
            status,
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
            fee_minor: fee_from(&response),
            redirect_url: redirect_url.clone(),
            requires_action: redirect_url.is_some() || pending,
            authorization_code: None,
            message: if token.is_none() {
                Some(
                    "Hyperswitch payment created unconfirmed; the shopper must complete it with the client_secret (Hyperswitch SDK or hosted checkout)"
                        .to_string(),
                )
            } else {
                None
            },
        })
    }

    pub(crate) async fn hyperswitch_capture(
        &self,
        provider_transaction_id: &str,
        amount_minor: Option<i64>,
    ) -> UnifiedResult<ChargeResult> {
        let url = self.url(&format!("/payments/{provider_transaction_id}/capture"))?;
        let mut body = json!({});
        if let Some(amount) = amount_minor {
            body["amount_to_capture"] = json!(amount);
        }
        let response = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .json(&body),
            "hyperswitch",
        )
        .await?;
        let amount = response
            .get("amount")
            .and_then(|value| value.as_i64())
            .unwrap_or_else(|| amount_minor.unwrap_or(0));
        let currency = response
            .get("currency")
            .and_then(|value| value.as_str())
            .unwrap_or("USD")
            .to_uppercase();
        let raw_status = json_str(&response, &["status"]).unwrap_or("unknown");
        Ok(ChargeResult {
            provider_transaction_id: provider_transaction_id.to_string(),
            status: map_status(PaymentProviderKind::Hyperswitch, raw_status),
            amount_minor: amount,
            currency,
            fee_minor: fee_from(&response),
            redirect_url: None,
            requires_action: false,
            authorization_code: None,
            message: None,
        })
    }

    pub(crate) async fn hyperswitch_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
        let url = self.url("/refunds")?;
        let mut body = json!({
            "payment_id": input.provider_transaction_id,
            "refund_type": "instant",
        });
        if let Some(amount) = input.amount_minor {
            body["amount"] = json!(amount);
        }
        if let Some(reason) = &input.reason {
            body["reason"] = json!(reason);
        }
        let response = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .header("X-Idempotency-Key", input.idempotency_key.clone())
                .json(&body),
            "hyperswitch",
        )
        .await?;
        let id = json_str(&response, &["refund_id"])
            .ok_or_else(|| UnifiedError::internal("Hyperswitch refund response has no refund_id"))?
            .to_string();
        let amount = response
            .get("amount")
            .and_then(|value| value.as_i64())
            .unwrap_or_else(|| input.amount_minor.unwrap_or(0));
        let raw_status = json_str(&response, &["status"]).unwrap_or("succeeded");
        let status = match raw_status.to_ascii_lowercase().as_str() {
            "succeeded" => PaymentStatus::Refunded,
            "pending" | "review" => PaymentStatus::Pending,
            _ => PaymentStatus::Failed,
        };
        Ok(RefundResult {
            provider_refund_id: id,
            status,
            amount_minor: amount,
            currency: input.currency.clone(),
        })
    }

    pub(crate) async fn hyperswitch_void(&self, provider_transaction_id: &str) -> UnifiedResult<()> {
        let url = self.url(&format!("/payments/{provider_transaction_id}/cancel"))?;
        send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .json(&json!({})),
            "hyperswitch",
        )
        .await?;
        Ok(())
    }

    /// Hyperswitch has no lightweight ping endpoint, so the
    /// connection test retrieves a synthetic payment id: 401/403
    /// means bad credentials, any other status means the API
    /// answered. Merchant-account endpoints need an admin API key,
    /// so they cannot be used for the test.
    pub(crate) async fn hyperswitch_test(&self) -> UnifiedResult<String> {
        let probe = format!("pay_connection_test_{}", Uuid::now_v7().simple());
        let url = self.url(&format!("/payments/{probe}"))?;
        let (status, _) = call_raw(
            self.client
                .request(Method::GET, url)
                .headers(self.auth_headers()?),
            "hyperswitch",
        )
        .await?;
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(UnifiedError::unauthorized(
                "Hyperswitch rejected the API key (HTTP 401/403)",
            ));
        }
        Ok(format!(
            "Hyperswitch API reachable; API key accepted (probe answered HTTP {status})"
        ))
    }

    /// Hyperswitch webhook verification: hex HMAC-SHA512 of the raw
    /// body with the profile's `payment_response_hash_key` (stored
    /// as `webhook_secret`), compared in constant time against the
    /// `X-Webhook-Signature-512` header.
    pub(crate) fn hyperswitch_webhook_verify(
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
                UnifiedError::bad_request("Hyperswitch connection needs webhook_secret to verify webhooks")
            })?;
        let signature = headers
            .get("X-Webhook-Signature-512")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| {
                UnifiedError::bad_request("Webhook is missing the X-Webhook-Signature-512 header")
            })?;
        let expected = hmac_sha512_hex(secret.as_bytes(), raw_body);
        Ok(digests_equal(&expected, signature))
    }

    /// Outcome of a verified Hyperswitch webhook, read from the
    /// envelope's `event_type`. The envelope is
    /// `{event_id, event_type, content: {type, object}}`; the body
    /// is authoritative once the signature is verified, so no
    /// re-fetch is needed. An unknown event type fails closed: the
    /// receiver must not apply an outcome it cannot map.
    pub(crate) fn hyperswitch_webhook_outcome(&self, raw_body: &[u8]) -> UnifiedResult<PaymentStatus> {
        let body: Value = serde_json::from_slice(raw_body).map_err(|_| {
            UnifiedError::bad_request("Hyperswitch webhook body is not valid JSON")
        })?;
        let event_type = json_str(&body, &["event_type"]).ok_or_else(|| {
            UnifiedError::bad_request("Hyperswitch webhook body carries no event_type")
        })?;
        match event_type {
            "payment_succeeded" => Ok(PaymentStatus::Captured),
            "payment_failed" => Ok(PaymentStatus::Failed),
            "payment_processing" => Ok(PaymentStatus::Pending),
            "payment_cancelled" => Ok(PaymentStatus::Cancelled),
            "payment_authorized" => Ok(PaymentStatus::Authorized),
            "refund_succeeded" => Ok(PaymentStatus::Refunded),
            "refund_failed" => Ok(PaymentStatus::Failed),
            "dispute_opened" => Ok(PaymentStatus::Chargeback),
            _ => Err(UnifiedError::bad_request(format!(
                "Hyperswitch webhook event type \"{event_type}\" is not mapped"
            ))),
        }
    }
}
