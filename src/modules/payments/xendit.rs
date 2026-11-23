//! payments - xendit.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::reference_id;
use reqwest::header::{HeaderMap};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, midtrans::*, hyperswitch::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



// ---------------------------------------------------------------------------
// Xendit.
// ---------------------------------------------------------------------------
impl ProviderAdapter {
    /// Xendit card charge (Charges API v2, one-time payment with a
    /// tokenized card). The provider reference is the order's
    /// reference when the caller threaded one, otherwise a generated
    /// reference in the shared format (see `charge_reference`).
    pub(crate) async fn xendit_charge(&self, input: &ChargeInput) -> UnifiedResult<ChargeResult> {
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
    pub(crate) async fn xendit_authorize(&self, input: &ChargeInput) -> UnifiedResult<ChargeResult> {
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

    pub(crate) fn xendit_charge_result(
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

    pub(crate) async fn xendit_capture(
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

    pub(crate) async fn xendit_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
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
    pub(crate) async fn xendit_payout(&self, input: &PayoutInput) -> UnifiedResult<PayoutResult> {
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

    pub(crate) async fn xendit_test(&self) -> UnifiedResult<String> {
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
    pub(crate) fn xendit_webhook_verify(
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
