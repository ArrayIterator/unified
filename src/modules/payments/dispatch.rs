//! payments - dispatch.

use unified_core::error::{UnifiedError, UnifiedResult};
use async_trait::async_trait;
use base64::Engine;
use chrono::{Utc};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Method};
use std::sync::{Arc};
use std::time::Duration;
use url::Url;
use uuid::Uuid;
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperswitch::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



// The dispatching adapter: one struct, one typed client per provider.
/// The built-in adapter. Each provider kind is served by its own
/// private `*_charge` / `*_refund` / ... function below, so the
/// per-provider request shapes stay typed and reviewable while the
/// registry, capability guards, and stores stay shared.
pub struct ProviderAdapter {
    pub(crate) kind: PaymentProviderKind,
    pub(crate) mode: GatewayMode,
    pub(crate) connection_id: Uuid,
    pub(crate) endpoint: Option<String>,
    pub(crate) credentials: PaymentCredentials,
    pub(crate) custom_config: Option<CustomGatewayConfig>,
    pub(crate) delegate_connection_id: Option<Uuid>,
    pub(crate) client: reqwest::Client,
}


impl ProviderAdapter {
    pub fn new(connection: &PaymentConnection) -> UnifiedResult<Self> {
        if connection.kind.needs_endpoint() && connection.endpoint.is_none() {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" needs an endpoint on the connection",
                connection.kind.as_str()
            )));
        }
        if let Some(endpoint) = &connection.endpoint {
            match connection.kind {
                PaymentProviderKind::Mastercard => validate_mastercard_endpoint(endpoint)?,
                PaymentProviderKind::Card | PaymentProviderKind::Custom => {
                    validate_payment_endpoint(Some(endpoint), connection.kind.as_str())?;
                }
                _ => {}
            }
        }
        Ok(Self {
            kind: connection.kind,
            mode: connection.mode,
            connection_id: connection.id,
            endpoint: connection.endpoint.clone(),
            credentials: connection.credentials.clone(),
            custom_config: connection.custom_config.clone(),
            delegate_connection_id: connection.delegate_connection_id,
            client: build_client()?,
        })
    }

    /// Base URL for the provider in the connection's mode, from the
    /// provider registry (`base_url`, or `sandbox_base_url` in
    /// sandbox mode when the provider has one). Endpoint kinds
    /// (Mastercard / Card / Custom) use the connection endpoint;
    /// wallets and bank transfer have no direct API base URL.
    pub(crate) fn base(&self) -> UnifiedResult<String> {
        match self.kind {
            PaymentProviderKind::GooglePay | PaymentProviderKind::ApplePay => {
                return Err(UnifiedError::bad_request(
                    "Wallet providers have no direct API base URL; charges run through the delegate connection",
                ));
            }
            PaymentProviderKind::BankTransfer => {
                return Err(UnifiedError::bad_request(
                    "Bank transfer has no API base URL; payments are recorded manually",
                ));
            }
            PaymentProviderKind::Mastercard
            | PaymentProviderKind::Card
            | PaymentProviderKind::Custom => {
                return self.endpoint.clone().ok_or_else(|| {
                    UnifiedError::bad_request(format!(
                        "Provider \"{}\" needs an endpoint on the connection",
                        self.kind.as_str()
                    ))
                });
            }
            _ => {}
        }
        let base = self
            .kind
            .definition()
            .and_then(|def| {
                def.effective_base_url(self.mode == GatewayMode::Sandbox)
                    .map(str::to_string)
            })
            .ok_or_else(|| {
                UnifiedError::internal(format!(
                    "Provider \"{}\" has no API base URL in the provider registry",
                    self.kind.as_str()
                ))
            })?;
        Ok(base.trim_end_matches('/').to_string())
    }

    /// Join a path onto the base URL and enforce the host allow-list.
    pub(crate) fn url(&self, path: &str) -> UnifiedResult<Url> {
        let base = self.base()?;
        let url = Url::parse(&format!("{base}{path}")).map_err(|_| {
            UnifiedError::internal(format!(
                "Cannot build {} request URL for path \"{path}\"",
                self.kind.as_str()
            ))
        })?;
        check_allowlist(&url, self.kind)?;
        Ok(url)
    }

    /// Synchronous auth headers for providers that use a static
    /// credential. PayPal uses `paypal_bearer` instead (OAuth2 token
    /// exchange), and Custom uses `custom_headers`.
    pub(crate) fn auth_headers(&self) -> UnifiedResult<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let kind = self.kind.as_str();
        match self.kind {
            PaymentProviderKind::Stripe => {
                let key = self
                    .credentials
                    .secret_key
                    .as_ref()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| UnifiedError::bad_request("Stripe connection needs secret_key"))?;
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| {
                        UnifiedError::internal("Stored Stripe credential is not a valid header value")
                    })?,
                );
            }
            PaymentProviderKind::Xendit => {
                let key = self
                    .credentials
                    .api_key
                    .as_ref()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| UnifiedError::bad_request("Xendit connection needs api_key"))?;
                let basic = base64::engine::general_purpose::STANDARD.encode(format!("{key}:"));
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Basic {basic}")).map_err(|_| {
                        UnifiedError::internal("Stored Xendit credential is not a valid header value")
                    })?,
                );
            }
            PaymentProviderKind::Midtrans => {
                let key = self
                    .credentials
                    .server_key
                    .as_ref()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| UnifiedError::bad_request("Midtrans connection needs server_key"))?;
                let basic = base64::engine::general_purpose::STANDARD.encode(format!("{key}:"));
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Basic {basic}")).map_err(|_| {
                        UnifiedError::internal("Stored Midtrans credential is not a valid header value")
                    })?,
                );
            }
            PaymentProviderKind::Hyperswitch => {
                let key = self
                    .credentials
                    .api_key
                    .as_ref()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| {
                        UnifiedError::bad_request("Hyperswitch connection needs api_key")
                    })?;
                headers.insert(
                    "api-key",
                    HeaderValue::from_str(key).map_err(|_| {
                        UnifiedError::internal("Stored Hyperswitch credential is not a valid header value")
                    })?,
                );
            }
            PaymentProviderKind::TwoCheckout => {
                let merchant = self
                    .credentials
                    .merchant_id
                    .as_ref()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| {
                        UnifiedError::bad_request("2Checkout connection needs merchant_id")
                    })?;
                let secret = self
                    .credentials
                    .secret_word
                    .as_ref()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| {
                        UnifiedError::bad_request("2Checkout connection needs secret_word")
                    })?;
                // 2Checkout REST authentication: the merchant code and
                // the current date are signed with the secret word
                // (HMAC-SHA256). The date format is the one 2CO
                // documents for this header (`YYYY-MM-DD HH:MM:SS`).
                let date = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                let hash = hmac_sha256_hex(secret.as_bytes(), format!("{merchant}{date}").as_bytes());
                headers.insert(
                    "X-Avangate-Authentication",
                    HeaderValue::from_str(&format!(
                        "code=\"{merchant}\" date=\"{date}\" hash=\"{hash}\""
                    ))
                    .map_err(|_| {
                        UnifiedError::internal("Stored 2Checkout credential is not a valid header value")
                    })?,
                );
            }
            PaymentProviderKind::Mastercard => {
                let merchant = self
                    .credentials
                    .merchant_id
                    .as_ref()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| {
                        UnifiedError::bad_request("Mastercard connection needs merchant_id")
                    })?;
                let password = self
                    .credentials
                    .secret_key
                    .as_ref()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| {
                        UnifiedError::bad_request("Mastercard connection needs secret_key")
                    })?;
                let basic = base64::engine::general_purpose::STANDARD
                    .encode(format!("merchant.{merchant}:{password}"));
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Basic {basic}")).map_err(|_| {
                        UnifiedError::internal("Stored Mastercard credential is not a valid header value")
                    })?,
                );
            }
            PaymentProviderKind::Card => {
                let key = self
                    .credentials
                    .api_key
                    .as_ref()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| UnifiedError::bad_request("Card connection needs api_key"))?;
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| {
                        UnifiedError::internal("Stored card processor credential is not a valid header value")
                    })?,
                );
            }
            PaymentProviderKind::Custom => {
                return self.custom_headers();
            }
            PaymentProviderKind::PayPal
            | PaymentProviderKind::GooglePay
            | PaymentProviderKind::ApplePay
            | PaymentProviderKind::BankTransfer => {
                return Err(UnifiedError::internal(format!(
                    "Provider \"{kind}\" does not use static auth headers"
                )));
            }
        }
        Ok(headers)
    }

    /// Auth headers for a custom gateway, per its configured auth type.
    pub(crate) fn custom_headers(&self) -> UnifiedResult<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let config = self.custom_config.as_ref().ok_or_else(|| {
            UnifiedError::bad_request("Custom provider connection needs custom_config")
        })?;
        let creds = &self.credentials;
        let header_value = |value: &str| {
            HeaderValue::from_str(value).map_err(|_| {
                UnifiedError::internal("Stored custom credential is not a valid header value")
            })
        };
        match config.auth_type.as_str() {
            "none" => {}
            "bearer" => {
                let key = creds.api_key.as_ref().filter(|v| !v.is_empty()).ok_or_else(|| {
                    UnifiedError::bad_request("Custom provider (bearer) needs api_key")
                })?;
                headers.insert(AUTHORIZATION, header_value(&format!("Bearer {key}"))?);
            }
            "basic" => {
                let username = creds.username.clone().unwrap_or_default();
                let password = creds.password.clone().unwrap_or_default();
                let basic = base64::engine::general_purpose::STANDARD
                    .encode(format!("{username}:{password}"));
                headers.insert(AUTHORIZATION, header_value(&format!("Basic {basic}"))?);
            }
            "api_key_header" => {
                let key = creds.api_key.as_ref().filter(|v| !v.is_empty()).ok_or_else(|| {
                    UnifiedError::bad_request("Custom provider (api_key_header) needs api_key")
                })?;
                headers.insert("X-Api-Key", header_value(key)?);
            }
            "api_key_query" => {
                // The key goes on the query string; see `custom_url`.
            }
            "hmac_sha256" => {
                let key = creds.api_key.as_ref().filter(|v| !v.is_empty()).ok_or_else(|| {
                    UnifiedError::bad_request("Custom provider (hmac_sha256) needs api_key")
                })?;
                headers.insert("X-Api-Key", header_value(key)?);
            }
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown custom auth type \"{other}\""
                )));
            }
        }
        Ok(headers)
    }

    /// A custom-gateway URL, adding the API key to the query string
    /// when the auth type is `api_key_query`.
    pub(crate) fn custom_url(&self, path: &str) -> UnifiedResult<Url> {
        let mut url = self.url(path)?;
        if self
            .custom_config
            .as_ref()
            .map(|config| config.auth_type.as_str())
            == Some("api_key_query")
        {
            let key = self
                .credentials
                .api_key
                .clone()
                .unwrap_or_default();
            url.query_pairs_mut().append_pair("api_key", &key);
        }
        Ok(url)
    }

    /// PayPal OAuth2 access token (client-credentials grant), cached
    /// in memory until shortly before expiry.
    pub(crate) async fn paypal_bearer(&self) -> UnifiedResult<String> {
        if let Some(entry) = paypal_tokens_store().get(&self.connection_id) {
            let (token, expires_at) = entry.value();
            if *expires_at > Utc::now() + chrono::Duration::seconds(60) {
                return Ok(token.clone());
            }
        }
        let client_id = self
            .credentials
            .client_id
            .clone()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| UnifiedError::bad_request("PayPal connection needs client_id"))?;
        let client_secret = self
            .credentials
            .client_secret
            .clone()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| UnifiedError::bad_request("PayPal connection needs client_secret"))?;
        let url = self.url("/v1/oauth2/token")?;
        let basic = base64::engine::general_purpose::STANDARD
            .encode(format!("{client_id}:{client_secret}"));
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .header(AUTHORIZATION, format!("Basic {basic}"))
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body("grant_type=client_credentials"),
            "paypal",
        )
        .await?;
        let token = body
            .get("access_token")
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                UnifiedError::internal("PayPal token exchange returned no access_token")
            })?
            .to_string();
        let expires_in = body
            .get("expires_in")
            .and_then(|value| value.as_i64())
            .unwrap_or(3600);
        paypal_tokens_store().insert(
            self.connection_id,
            (
                token.clone(),
                Utc::now() + chrono::Duration::seconds(expires_in),
            ),
        );
        Ok(token)
    }

    /// The delegate adapter for a wallet provider.
    pub(crate) fn delegate(&self) -> UnifiedResult<Arc<dyn PaymentProvider>> {
        let delegate_id = self.delegate_connection_id.ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Provider \"{}\" needs delegate_connection_id",
                self.kind.as_str()
            ))
        })?;
        let delegate = connections_store()
            .get(&delegate_id)
            .map(|entry| entry.clone())
            .ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Delegate connection \"{delegate_id}\" does not exist"
                ))
            })?;
        if !delegate.enabled {
            return Err(UnifiedError::bad_request(
                "Delegate connection is disabled",
            ));
        }
        payment_registry().build(&delegate)
    }

    /// Resolve the delegate's currency list for a wallet charge, and
    /// reject a currency the delegate cannot charge in.
    pub(crate) fn wallet_currency(&self, currency: &str) -> UnifiedResult<String> {
        let code = validate_currency(currency)?;
        let delegate_id = self.delegate_connection_id.ok_or_else(|| {
            UnifiedError::bad_request("Wallet provider needs delegate_connection_id")
        })?;
        let Some(delegate) = connections_store().get(&delegate_id) else {
            return Err(UnifiedError::bad_request(
                "Delegate connection does not exist",
            ));
        };
        if !currencies_for(delegate.kind).contains(&code.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Currency \"{code}\" is not supported by the delegate gateway \"{}\"",
                delegate.kind.as_str()
            )));
        }
        Ok(code)
    }
}



#[async_trait]
// `PaymentProvider` implementation: capability guards + dispatch.
impl PaymentProvider for ProviderAdapter {
    fn kind(&self) -> PaymentProviderKind {
        self.kind
    }

    fn capabilities(&self) -> PaymentCapabilities {
        match (self.kind, &self.custom_config) {
            (PaymentProviderKind::Custom, Some(config)) => config.capabilities(),
            _ => capabilities_for(self.kind),
        }
    }

    async fn test_connection(&self) -> UnifiedResult<ConnectionTestResult> {
        let started = std::time::Instant::now();
        let detail = match self.kind {
            PaymentProviderKind::Stripe => self.stripe_test().await?,
            PaymentProviderKind::PayPal => self.paypal_test().await?,
            PaymentProviderKind::Xendit => self.xendit_test().await?,
            PaymentProviderKind::Midtrans => self.midtrans_test().await?,
            PaymentProviderKind::Hyperswitch => self.hyperswitch_test().await?,
            PaymentProviderKind::TwoCheckout => self.two_checkout_test().await?,
            PaymentProviderKind::Mastercard => self.mastercard_test().await?,
            PaymentProviderKind::Card => self.card_test().await?,
            PaymentProviderKind::Custom => self.custom_test().await?,
            PaymentProviderKind::GooglePay | PaymentProviderKind::ApplePay => {
                self.wallet_test().await?
            }
            PaymentProviderKind::BankTransfer => self.bank_transfer_test().await?,
        };
        Ok(ConnectionTestResult {
            kind: self.kind.as_str().to_string(),
            mode: self.mode.as_str().to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            detail,
        })
    }

    async fn charge(&self, input: &ChargeInput) -> UnifiedResult<ChargeResult> {
        if !self.capabilities().supports_charge {
            return Err(unsupported(self.kind, "charge", "supports_charge"));
        }
        validate_money(input.amount_minor, &input.currency)?;
        match self.kind {
            PaymentProviderKind::Stripe => self.stripe_charge(input, false).await,
            PaymentProviderKind::PayPal => self.paypal_charge(input, false).await,
            PaymentProviderKind::Xendit => self.xendit_charge(input).await,
            PaymentProviderKind::Midtrans => self.midtrans_charge(input).await,
            PaymentProviderKind::Hyperswitch => self.hyperswitch_charge(input, false).await,
            PaymentProviderKind::TwoCheckout => self.two_checkout_charge(input).await,
            PaymentProviderKind::Mastercard => self.mastercard_charge(input, "PAY").await,
            PaymentProviderKind::Card => self.card_charge(input, "charge").await,
            PaymentProviderKind::Custom => self.custom_charge(input, "charge").await,
            PaymentProviderKind::GooglePay | PaymentProviderKind::ApplePay => {
                self.wallet_charge(input).await
            }
            PaymentProviderKind::BankTransfer => self.bank_transfer_charge(input).await,
        }
    }

    async fn authorize(&self, input: &ChargeInput) -> UnifiedResult<ChargeResult> {
        if !self.capabilities().supports_authorize {
            return Err(unsupported(self.kind, "authorize", "supports_authorize"));
        }
        validate_money(input.amount_minor, &input.currency)?;
        match self.kind {
            PaymentProviderKind::Stripe => self.stripe_charge(input, true).await,
            PaymentProviderKind::PayPal => self.paypal_charge(input, true).await,
            PaymentProviderKind::Xendit => self.xendit_authorize(input).await,
            PaymentProviderKind::Midtrans => self.midtrans_authorize(input).await,
            PaymentProviderKind::Hyperswitch => self.hyperswitch_charge(input, true).await,
            PaymentProviderKind::Mastercard => self.mastercard_charge(input, "AUTHORIZE").await,
            PaymentProviderKind::Card => self.card_charge(input, "authorize").await,
            PaymentProviderKind::Custom => self.custom_charge(input, "authorize").await,
            PaymentProviderKind::BankTransfer => self.bank_transfer_authorize(input).await,
            _ => Err(unsupported(self.kind, "authorize", "supports_authorize")),
        }
    }

    async fn capture(
        &self,
        provider_transaction_id: &str,
        amount_minor: Option<i64>,
    ) -> UnifiedResult<ChargeResult> {
        if !self.capabilities().supports_capture {
            return Err(unsupported(self.kind, "capture", "supports_capture"));
        }
        if provider_transaction_id.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "A provider transaction id is required to capture",
            ));
        }
        if let Some(amount) = amount_minor {
            if amount <= 0 {
                return Err(UnifiedError::bad_request(
                    "Capture amount must be greater than zero",
                ));
            }
        }
        match self.kind {
            PaymentProviderKind::Stripe => {
                self.stripe_capture(provider_transaction_id, amount_minor).await
            }
            PaymentProviderKind::PayPal => {
                self.paypal_capture_authorization(provider_transaction_id, amount_minor)
                    .await
            }
            PaymentProviderKind::Xendit => {
                self.xendit_capture(provider_transaction_id, amount_minor).await
            }
            PaymentProviderKind::Midtrans => {
                self.midtrans_capture(provider_transaction_id).await
            }
            PaymentProviderKind::Hyperswitch => {
                self.hyperswitch_capture(provider_transaction_id, amount_minor).await
            }
            PaymentProviderKind::Mastercard => {
                self.mastercard_capture(provider_transaction_id, amount_minor).await
            }
            PaymentProviderKind::Card => {
                self.card_capture(provider_transaction_id, amount_minor).await
            }
            PaymentProviderKind::Custom => {
                self.custom_capture(provider_transaction_id, amount_minor).await
            }
            _ => Err(unsupported(self.kind, "capture", "supports_capture")),
        }
    }

    async fn refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
        if !self.capabilities().supports_refund {
            return Err(unsupported(self.kind, "refund", "supports_refund"));
        }
        if let Some(amount) = input.amount_minor {
            if amount <= 0 {
                return Err(UnifiedError::bad_request(
                    "Refund amount must be greater than zero",
                ));
            }
        }
        validate_currency(&input.currency)?;
        match self.kind {
            PaymentProviderKind::Stripe => self.stripe_refund(input).await,
            PaymentProviderKind::PayPal => self.paypal_refund(input).await,
            PaymentProviderKind::Xendit => self.xendit_refund(input).await,
            PaymentProviderKind::Midtrans => self.midtrans_refund(input).await,
            PaymentProviderKind::Hyperswitch => self.hyperswitch_refund(input).await,
            PaymentProviderKind::TwoCheckout => self.two_checkout_refund(input).await,
            PaymentProviderKind::Mastercard => self.mastercard_refund(input).await,
            PaymentProviderKind::Card => self.card_refund(input).await,
            PaymentProviderKind::Custom => self.custom_refund(input).await,
            PaymentProviderKind::GooglePay | PaymentProviderKind::ApplePay => {
                self.wallet_refund(input).await
            }
            PaymentProviderKind::BankTransfer => self.bank_transfer_refund(input).await,
        }
    }

    async fn void(&self, provider_transaction_id: &str) -> UnifiedResult<()> {
        if !self.capabilities().supports_void {
            return Err(unsupported(self.kind, "void", "supports_void"));
        }
        if provider_transaction_id.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "A provider transaction id is required to void",
            ));
        }
        match self.kind {
            PaymentProviderKind::Stripe => {
                self.stripe_void(provider_transaction_id).await
            }
            PaymentProviderKind::PayPal => {
                self.paypal_void(provider_transaction_id).await
            }
            PaymentProviderKind::Hyperswitch => {
                self.hyperswitch_void(provider_transaction_id).await
            }
            PaymentProviderKind::Mastercard => {
                self.mastercard_void(provider_transaction_id).await
            }
            PaymentProviderKind::Card => self.card_void(provider_transaction_id).await,
            PaymentProviderKind::Custom => self.custom_void(provider_transaction_id).await,
            PaymentProviderKind::BankTransfer => {
                self.bank_transfer_void(provider_transaction_id).await
            }
            _ => Err(unsupported(self.kind, "void", "supports_void")),
        }
    }

    async fn subscription_create(
        &self,
        input: &SubscriptionInput,
    ) -> UnifiedResult<SubscriptionResult> {
        if !self.capabilities().supports_subscriptions {
            return Err(unsupported(
                self.kind,
                "subscription_create",
                "supports_subscriptions",
            ));
        }
        validate_money(input.amount_minor, &input.currency)?;
        if !matches!(input.interval.as_str(), "day" | "week" | "month" | "year") {
            return Err(UnifiedError::bad_request(
                "Subscription interval must be day, week, month, or year",
            ));
        }
        match self.kind {
            PaymentProviderKind::Stripe => self.stripe_subscription_create(input).await,
            PaymentProviderKind::PayPal => self.paypal_subscription_create(input).await,
            PaymentProviderKind::Midtrans => self.midtrans_subscription_create(input).await,
            PaymentProviderKind::Custom => self.custom_subscription_create(input).await,
            _ => self.local_schedule_subscription_create(input).await,
        }
    }

    async fn subscription_cancel(&self, provider_subscription_id: &str) -> UnifiedResult<()> {
        if !self.capabilities().supports_subscriptions {
            return Err(unsupported(
                self.kind,
                "subscription_cancel",
                "supports_subscriptions",
            ));
        }
        if provider_subscription_id.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "A provider subscription id is required to cancel",
            ));
        }
        match self.kind {
            PaymentProviderKind::Stripe => {
                self.stripe_subscription_cancel(provider_subscription_id)
                    .await
            }
            PaymentProviderKind::PayPal => {
                self.paypal_subscription_cancel(provider_subscription_id)
                    .await
            }
            PaymentProviderKind::Midtrans => {
                self.midtrans_subscription_cancel(provider_subscription_id)
                    .await
            }
            PaymentProviderKind::Custom => {
                self.custom_subscription_cancel(provider_subscription_id)
                    .await
            }
            // Local-schedule providers have no provider-side object to
            // cancel; the subscription record is cancelled by the
            // handler, which stops future renewal charges.
            _ => Ok(()),
        }
    }

    async fn webhook_verify(
        &self,
        headers: &HeaderMap,
        raw_body: &[u8],
    ) -> UnifiedResult<bool> {
        match self.kind {
            PaymentProviderKind::Stripe => self.stripe_webhook_verify(headers, raw_body),
            PaymentProviderKind::PayPal => {
                self.paypal_webhook_verify(headers, raw_body).await
            }
            PaymentProviderKind::Xendit => self.xendit_webhook_verify(headers, raw_body),
            PaymentProviderKind::Midtrans => self.midtrans_webhook_verify(raw_body),
            PaymentProviderKind::Hyperswitch => {
                self.hyperswitch_webhook_verify(headers, raw_body)
            }
            PaymentProviderKind::TwoCheckout => {
                self.two_checkout_webhook_verify(raw_body).await
            }
            PaymentProviderKind::Mastercard => {
                self.mastercard_webhook_verify(headers, raw_body)
            }
            PaymentProviderKind::Card => self.card_webhook_verify(headers, raw_body),
            PaymentProviderKind::Custom => self.custom_webhook_verify(headers, raw_body),
            PaymentProviderKind::GooglePay | PaymentProviderKind::ApplePay => Err(
                UnifiedError::bad_request(
                    "Wallet providers have no server webhook of their own; payment events arrive through the delegate gateway's webhook",
                ),
            ),
            PaymentProviderKind::BankTransfer => Err(UnifiedError::bad_request(
                "Bank transfer has no provider webhook; verification is a manual administrator step in the bank-transfer module",
            )),
        }
    }

    async fn webhook_outcome(&self, raw_body: &[u8]) -> UnifiedResult<Option<PaymentStatus>> {
        match self.kind {
            PaymentProviderKind::Hyperswitch => self
                .hyperswitch_webhook_outcome(raw_body)
                .map(Some),
            PaymentProviderKind::TwoCheckout => self
                .two_checkout_webhook_outcome(raw_body)
                .await
                .map(Some),
            _ => Ok(None),
        }
    }

    async fn payout(&self, input: &PayoutInput) -> UnifiedResult<PayoutResult> {
        if !self.capabilities().supports_payouts {
            return Err(unsupported(self.kind, "payout", "supports_payouts"));
        }
        validate_money(input.amount_minor, &input.currency)?;
        if input.destination_account.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "A payout destination account is required",
            ));
        }
        match self.kind {
            PaymentProviderKind::Stripe => self.stripe_payout(input).await,
            PaymentProviderKind::PayPal => self.paypal_payout(input).await,
            PaymentProviderKind::Xendit => self.xendit_payout(input).await,
            PaymentProviderKind::Custom => self.custom_payout(input).await,
            _ => Err(unsupported(self.kind, "payout", "supports_payouts")),
        }
    }
}
