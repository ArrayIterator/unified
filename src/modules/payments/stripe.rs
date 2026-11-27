//! payments - stripe.

use unified_core::error::{UnifiedError, UnifiedResult};
use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap};
use reqwest::{Method, StatusCode};
use std::collections::HashMap;
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, paypal::*, xendit::*, midtrans::*, hyperswitch::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



// Stripe.
impl ProviderAdapter {
    /// Stripe charge / authorize. Stripe's API is form-encoded.
    /// `authorize` creates the PaymentIntent with manual capture.
    pub(crate) async fn stripe_charge(
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

    pub(crate) async fn stripe_capture(
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

    pub(crate) async fn stripe_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
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

    pub(crate) async fn stripe_void(&self, provider_transaction_id: &str) -> UnifiedResult<()> {
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

    pub(crate) async fn stripe_subscription_create(
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

    pub(crate) async fn stripe_subscription_cancel(&self, provider_subscription_id: &str) -> UnifiedResult<()> {
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

    pub(crate) async fn stripe_payout(&self, input: &PayoutInput) -> UnifiedResult<PayoutResult> {
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

    pub(crate) async fn stripe_test(&self) -> UnifiedResult<String> {
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
    pub(crate) fn stripe_webhook_verify(
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
