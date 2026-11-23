//! payments - paypal.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use reqwest::header::{HeaderMap, AUTHORIZATION};
use reqwest::{Method};
use serde_json::{json, Value};
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, xendit::*, midtrans::*, hyperpay::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



// ---------------------------------------------------------------------------
// PayPal.
// ---------------------------------------------------------------------------
impl ProviderAdapter {
    /// PayPal charge / authorize. With a payment method token the order
    /// is created and captured (or authorized) in one call chain;
    /// without a token the order is created and the caller must send
    /// the shopper to the approval link (status pending).
    pub(crate) async fn paypal_charge(
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
    pub(crate) async fn paypal_capture_authorization(
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

    pub(crate) async fn paypal_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
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

    pub(crate) async fn paypal_void(&self, provider_transaction_id: &str) -> UnifiedResult<()> {
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

    pub(crate) async fn paypal_subscription_create(
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

    pub(crate) async fn paypal_subscription_cancel(&self, provider_subscription_id: &str) -> UnifiedResult<()> {
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

    pub(crate) async fn paypal_payout(&self, input: &PayoutInput) -> UnifiedResult<PayoutResult> {
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

    pub(crate) async fn paypal_test(&self) -> UnifiedResult<String> {
        // The token exchange itself is the connection test: a bad
        // client id / secret fails here with 401.
        let _token = self.paypal_bearer().await?;
        Ok("PayPal API reachable; client credentials accepted".to_string())
    }

    /// PayPal webhook verification: server-side, through PayPal's
    /// verify-webhook-signature endpoint, using the transmission
    /// headers and the configured webhook id.
    pub(crate) async fn paypal_webhook_verify(
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
