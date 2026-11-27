//! payments - custom provider.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use base64::Engine;
use reqwest::header::{HeaderMap, AUTHORIZATION};
use reqwest::{Method, StatusCode};
use serde_json::{json};
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperswitch::*, twocheckout::*, mpgs::*, generic_card::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



// Custom provider.
impl ProviderAdapter {
    pub(crate) fn custom_config(&self) -> UnifiedResult<&CustomGatewayConfig> {
        self.custom_config.as_ref().ok_or_else(|| {
            UnifiedError::bad_request("Custom provider connection needs custom_config")
        })
    }

    /// Custom charge / authorize: POST the configured path with the
    /// standard envelope. The response transaction id and status come
    /// from the configured field names.
    pub(crate) async fn custom_charge(
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

    pub(crate) async fn custom_capture(
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

    pub(crate) async fn custom_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
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

    pub(crate) async fn custom_void(&self, provider_transaction_id: &str) -> UnifiedResult<()> {
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

    pub(crate) async fn custom_subscription_create(
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

    pub(crate) async fn custom_subscription_cancel(&self, provider_subscription_id: &str) -> UnifiedResult<()> {
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

    pub(crate) async fn custom_payout(&self, input: &PayoutInput) -> UnifiedResult<PayoutResult> {
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
    pub(crate) async fn custom_test(&self) -> UnifiedResult<String> {
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
    pub(crate) fn custom_webhook_verify(
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
