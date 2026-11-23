//! payments - mpgs.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::reference_id;
use reqwest::header::{HeaderMap};
use reqwest::{Method, StatusCode};
use serde_json::{json};
use uuid::Uuid;
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperpay::*, twocheckout::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



// ---------------------------------------------------------------------------
// Mastercard gateway (MPGS).
// ---------------------------------------------------------------------------
impl ProviderAdapter {
    /// Mastercard gateway transaction. `operation` is one of `PAY`,
    /// `AUTHORIZE`, `CAPTURE`, `REFUND`, `VOID`. Amounts are major
    /// units as decimals. The order id is the caller-supplied order id
    /// when present, otherwise a generated one; the transaction id is
    /// generated per call and returned as the provider transaction id.
    pub(crate) async fn mastercard_operation(
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
                // MPGS takes the amount as a decimal string; the
                // integer minor units convert exactly, with no
                // float in between.
                "amount": major_amount(input.amount_minor, &input.currency),
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

    pub(crate) async fn mastercard_charge(
        &self,
        input: &ChargeInput,
        operation: &str,
    ) -> UnifiedResult<ChargeResult> {
        self.mastercard_operation(operation, input, None).await
    }

    pub(crate) async fn mastercard_capture(
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

    pub(crate) async fn mastercard_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
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

    pub(crate) async fn mastercard_void(&self, provider_transaction_id: &str) -> UnifiedResult<()> {
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
    pub(crate) async fn mastercard_test(&self) -> UnifiedResult<String> {
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
    pub(crate) fn mastercard_webhook_verify(
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
