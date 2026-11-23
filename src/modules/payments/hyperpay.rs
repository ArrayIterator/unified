//! payments - hyperpay.

use unified_core::error::{UnifiedError, UnifiedResult};
use reqwest::{Method, StatusCode};
use serde_json::{Value};
use std::collections::HashMap;
use uuid::Uuid;
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



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
    pub(crate) async fn hyperpay_charge(
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
    pub(crate) async fn hyperpay_payment_action(
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

    pub(crate) async fn hyperpay_capture(
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

    pub(crate) async fn hyperpay_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
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

    pub(crate) async fn hyperpay_void(&self, provider_transaction_id: &str) -> UnifiedResult<()> {
        // No amount is sent for a void, so no currency is needed.
        self.hyperpay_payment_action(provider_transaction_id, "RV", None, "USD")
            .await?;
        Ok(())
    }

    /// HyperPay has no lightweight ping endpoint, so the connection
    /// test queries a synthetic checkout id: 401/403 means bad
    /// credentials, any other status means the API answered.
    pub(crate) async fn hyperpay_test(&self) -> UnifiedResult<String> {
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
    pub(crate) async fn hyperpay_fetch_payment(
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

    pub(crate) async fn hyperpay_webhook_verify(&self, raw_body: &[u8]) -> UnifiedResult<bool> {
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
    pub(crate) async fn hyperpay_webhook_outcome(&self, raw_body: &[u8]) -> UnifiedResult<PaymentStatus> {
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
pub(crate) fn hyperpay_status(result_code: &str) -> PaymentStatus {
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
