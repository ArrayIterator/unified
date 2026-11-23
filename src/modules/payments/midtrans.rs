//! payments - midtrans.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::reference_id;
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use uuid::Uuid;
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, hyperpay::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



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
    pub(crate) async fn midtrans_charge(&self, input: &ChargeInput) -> UnifiedResult<ChargeResult> {
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
    pub(crate) async fn midtrans_authorize(&self, input: &ChargeInput) -> UnifiedResult<ChargeResult> {
        let mut input = input.clone();
        input.three_ds = true;
        self.midtrans_charge(&input).await
    }

    /// Read the captured amount from a Midtrans capture response.
    /// Midtrans returns gross_amount as a decimal string
    /// ("10000.00") or a number; either way it is major units.
    /// The amount is converted from the decimal string with
    /// integer arithmetic, never through f64, and a missing or
    /// unreadable amount is an error — never a silent zero.
    pub(crate) fn midtrans_capture_amount(body: &Value) -> UnifiedResult<(i64, String)> {
        let currency = body
            .get("currency")
            .and_then(|value| value.as_str())
            .unwrap_or("IDR")
            .to_string();
        let amount_text = body
            .get("gross_amount")
            .and_then(|value| {
                value.as_str().map(str::to_string).or_else(|| {
                    // A JSON number renders back in its original
                    // decimal form, so no float ever holds it.
                    value.is_number().then(|| value.to_string())
                })
            })
            .ok_or_else(|| {
                UnifiedError::internal("Midtrans capture response has no gross_amount")
            })?;
        let amount = decimal_str_to_minor(&amount_text, &currency).map_err(|_| {
            UnifiedError::internal(format!(
                "Midtrans capture response has an unreadable gross_amount \"{amount_text}\""
            ))
        })?;
        Ok((amount, currency))
    }

    pub(crate) async fn midtrans_capture(&self, provider_transaction_id: &str) -> UnifiedResult<ChargeResult> {
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
        let (amount, currency) = ProviderAdapter::midtrans_capture_amount(&body)?;
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

    pub(crate) async fn midtrans_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
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

    pub(crate) async fn midtrans_subscription_create(
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

    pub(crate) async fn midtrans_subscription_cancel(&self, provider_subscription_id: &str) -> UnifiedResult<()> {
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
    pub(crate) async fn midtrans_test(&self) -> UnifiedResult<String> {
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
    pub(crate) fn midtrans_webhook_verify(&self, raw_body: &[u8]) -> UnifiedResult<bool> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_amount_parses_decimals_without_floats_or_silent_zero() {
        // Decimal string in a zero-decimal currency.
        let body = serde_json::json!({ "gross_amount": "10000.00", "currency": "IDR" });
        assert_eq!(ProviderAdapter::midtrans_capture_amount(&body).unwrap(), (10000, "IDR".to_string()));
        // Decimal string in a two-decimal currency.
        let body = serde_json::json!({ "gross_amount": "10.50", "currency": "USD" });
        assert_eq!(ProviderAdapter::midtrans_capture_amount(&body).unwrap(), (1050, "USD".to_string()));
        // JSON number, rendered back as its decimal text.
        let body = serde_json::json!({ "gross_amount": 2500, "currency": "IDR" });
        assert_eq!(ProviderAdapter::midtrans_capture_amount(&body).unwrap(), (2500, "IDR".to_string()));
        // A missing or unreadable amount is an error, never zero.
        let body = serde_json::json!({ "currency": "IDR" });
        assert!(ProviderAdapter::midtrans_capture_amount(&body).is_err());
        let body = serde_json::json!({ "gross_amount": "not-an-amount", "currency": "IDR" });
        assert!(ProviderAdapter::midtrans_capture_amount(&body).is_err());
    }
}
