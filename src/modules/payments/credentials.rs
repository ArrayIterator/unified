//! payments - credentials.

use unified_core::error::{UnifiedError, UnifiedResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperpay::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
// ---------------------------------------------------------------------------
// Credentials and connections.
// ---------------------------------------------------------------------------
/// Credentials for one connection. Every field is optional at the type
/// level; `validate_connection` enforces the per-kind required set.
/// Values are write-only: they are never serialized into a view, never
/// logged, and never included in hook payloads.
pub struct PaymentCredentials {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub secret_key: Option<String>,
    #[serde(default)]
    pub public_key: Option<String>,
    #[serde(default)]
    pub merchant_id: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub bearer_token: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub webhook_secret: Option<String>,
    #[serde(default)]
    pub webhook_id: Option<String>,
    #[serde(default)]
    pub callback_token: Option<String>,
    #[serde(default)]
    pub server_key: Option<String>,
    #[serde(default)]
    pub client_key: Option<String>,
    #[serde(default)]
    pub secret_word: Option<String>,
}


impl PaymentCredentials {
    /// Names of the credential fields that are set (non-empty), for the
    /// masked read view. Values are never exposed.
    pub(crate) fn configured_fields(&self) -> Vec<&'static str> {
        let mut fields = Vec::new();
        let present = |value: &Option<String>| {
            value.as_deref().map(str::is_empty).unwrap_or(true) == false
        };
        if present(&self.api_key) {
            fields.push("api_key");
        }
        if present(&self.secret_key) {
            fields.push("secret_key");
        }
        if present(&self.public_key) {
            fields.push("public_key");
        }
        if present(&self.merchant_id) {
            fields.push("merchant_id");
        }
        if present(&self.client_id) {
            fields.push("client_id");
        }
        if present(&self.client_secret) {
            fields.push("client_secret");
        }
        if present(&self.access_token) {
            fields.push("access_token");
        }
        if present(&self.bearer_token) {
            fields.push("bearer_token");
        }
        if present(&self.username) {
            fields.push("username");
        }
        if present(&self.password) {
            fields.push("password");
        }
        if present(&self.webhook_secret) {
            fields.push("webhook_secret");
        }
        if present(&self.webhook_id) {
            fields.push("webhook_id");
        }
        if present(&self.callback_token) {
            fields.push("callback_token");
        }
        if present(&self.server_key) {
            fields.push("server_key");
        }
        if present(&self.client_key) {
            fields.push("client_key");
        }
        if present(&self.secret_word) {
            fields.push("secret_word");
        }
        fields
    }

    /// Merge an update into these credentials: only fields present in
    /// the update overwrite the stored value, so a partial update
    /// never wipes credentials the caller did not resend.
    pub(crate) fn merge(&mut self, update: PaymentCredentials) {
        if update.api_key.is_some() {
            self.api_key = update.api_key;
        }
        if update.secret_key.is_some() {
            self.secret_key = update.secret_key;
        }
        if update.public_key.is_some() {
            self.public_key = update.public_key;
        }
        if update.merchant_id.is_some() {
            self.merchant_id = update.merchant_id;
        }
        if update.client_id.is_some() {
            self.client_id = update.client_id;
        }
        if update.client_secret.is_some() {
            self.client_secret = update.client_secret;
        }
        if update.access_token.is_some() {
            self.access_token = update.access_token;
        }
        if update.bearer_token.is_some() {
            self.bearer_token = update.bearer_token;
        }
        if update.username.is_some() {
            self.username = update.username;
        }
        if update.password.is_some() {
            self.password = update.password;
        }
        if update.webhook_secret.is_some() {
            self.webhook_secret = update.webhook_secret;
        }
        if update.webhook_id.is_some() {
            self.webhook_id = update.webhook_id;
        }
        if update.callback_token.is_some() {
            self.callback_token = update.callback_token;
        }
        if update.server_key.is_some() {
            self.server_key = update.server_key;
        }
        if update.client_key.is_some() {
            self.client_key = update.client_key;
        }
        if update.secret_word.is_some() {
            self.secret_word = update.secret_word;
        }
    }
}


/// Sandbox or live mode for a connection. Credentials are per mode in
/// practice: switching mode does not rewrite credentials, it only
/// changes which base URL the adapter uses, so the operator must store
/// the matching credentials for the active mode. The mode endpoint
/// exists so the switch is explicit and auditable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayMode {
    Sandbox,
    Live,
}


impl GatewayMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Sandbox => "sandbox",
            Self::Live => "live",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "sandbox" | "test" => Some(Self::Sandbox),
            "live" | "production" => Some(Self::Live),
            _ => None,
        }
    }
}


/// Config-driven definition for `PaymentProviderKind::Custom` (and the
/// endpoint contract for `PaymentProviderKind::Card`). Contains no
/// credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomGatewayConfig {
    /// `none`, `bearer`, `basic`, `api_key_header`, `api_key_query`,
    /// or `hmac_sha256`.
    pub auth_type: String,
    /// Path of the charge endpoint, for example `/v1/charges`.
    pub charge_path: String,
    #[serde(default)]
    pub authorize_path: Option<String>,
    #[serde(default)]
    pub capture_path: Option<String>,
    #[serde(default)]
    pub refund_path: Option<String>,
    #[serde(default)]
    pub void_path: Option<String>,
    #[serde(default)]
    pub subscription_path: Option<String>,
    #[serde(default)]
    pub payout_path: Option<String>,
    #[serde(default)]
    pub test_path: Option<String>,
    /// Header carrying the webhook signature, default `X-Signature`.
    #[serde(default)]
    pub signature_header: Option<String>,
    /// JSON field holding the transaction id in responses,
    /// default `id`.
    #[serde(default)]
    pub transaction_id_field: Option<String>,
    /// JSON field holding the status in responses, default `status`.
    #[serde(default)]
    pub status_field: Option<String>,
    /// Capabilities this custom gateway actually offers. A missing
    /// endpoint path disables the matching capability.
    #[serde(default)]
    pub supports_authorize: bool,
    #[serde(default)]
    pub supports_capture: bool,
    #[serde(default = "default_true")]
    pub supports_refund: bool,
    #[serde(default)]
    pub supports_void: bool,
    #[serde(default)]
    pub supports_subscriptions: bool,
    #[serde(default)]
    pub supports_payouts: bool,
}


impl Default for CustomGatewayConfig {
    fn default() -> Self {
        Self {
            auth_type: "bearer".to_string(),
            charge_path: "/v1/charges".to_string(),
            authorize_path: None,
            capture_path: None,
            refund_path: None,
            void_path: None,
            subscription_path: None,
            payout_path: None,
            test_path: None,
            signature_header: None,
            transaction_id_field: None,
            status_field: None,
            supports_authorize: false,
            supports_capture: false,
            supports_refund: true,
            supports_void: false,
            supports_subscriptions: false,
            supports_payouts: false,
        }
    }
}


pub(crate) fn default_true() -> bool {
    true
}


impl CustomGatewayConfig {
    pub(crate) fn capabilities(&self) -> PaymentCapabilities {
        PaymentCapabilities {
            supports_authorize: self.supports_authorize && self.authorize_path.is_some(),
            supports_capture: self.supports_capture && self.capture_path.is_some(),
            supports_charge: true,
            supports_refund: self.supports_refund && self.refund_path.is_some(),
            supports_void: self.supports_void && self.void_path.is_some(),
            supports_subscriptions: self.supports_subscriptions
                && self.subscription_path.is_some(),
            supports_payouts: self.supports_payouts && self.payout_path.is_some(),
            supports_3ds: false,
            supports_webhooks: true,
            subscriptions_are_local_schedule: false,
        }
    }
}


/// One stored provider connection. Kept in memory (same pattern as the
/// provider registry and the store); durable persistence is a
/// follow-up for the integrator — see the migration sketch in the
/// coordinator report.
#[derive(Debug, Clone)]
pub struct PaymentConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: PaymentProviderKind,
    pub mode: GatewayMode,
    /// API / processor endpoint for Card, Mastercard, and Custom.
    /// Empty for fixed-host providers.
    pub endpoint: Option<String>,
    pub credentials: PaymentCredentials,
    /// Config-driven definition for `PaymentProviderKind::Custom`
    /// (`None` for every other kind).
    pub custom_config: Option<CustomGatewayConfig>,
    /// Wallet adapters only: the gateway connection that performs the
    /// charge for a wallet token.
    pub delegate_connection_id: Option<Uuid>,
    pub enabled: bool,
    pub last_tested_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}


/// Masked read view of a connection: credentials are never returned.
#[derive(Debug, Clone, Serialize)]
pub struct PaymentConnectionView {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub kind_display: String,
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Names of credential fields that are configured (values hidden).
    pub configured_credentials: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_config: Option<CustomGatewayConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delegate_connection_id: Option<Uuid>,
    pub capabilities: PaymentCapabilities,
    pub currencies: Vec<String>,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_tested_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}


impl PaymentConnection {
    pub(crate) fn view(&self) -> PaymentConnectionView {
        let capabilities = match (self.kind, &self.custom_config) {
            (PaymentProviderKind::Custom, Some(config)) => config.capabilities(),
            _ => capabilities_for(self.kind),
        };
        PaymentConnectionView {
            id: self.id,
            name: self.name.clone(),
            kind: self.kind.as_str().to_string(),
            kind_display: self.kind.display_name().to_string(),
            mode: self.mode.as_str().to_string(),
            endpoint: self.endpoint.clone(),
            configured_credentials: self
                .credentials
                .configured_fields()
                .into_iter()
                .map(str::to_string)
                .collect(),
            custom_config: self.custom_config.clone(),
            delegate_connection_id: self.delegate_connection_id,
            capabilities,
            currencies: currencies_for(self.kind)
                .iter()
                .map(|currency| currency.to_string())
                .collect(),
            enabled: self.enabled,
            last_tested_at: self.last_tested_at,
            last_status: self.last_status.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}



// ---------------------------------------------------------------------------
// Connection validation.
// ---------------------------------------------------------------------------
/// Validate a user-supplied API endpoint: https only (money must never
/// travel over plain http), a host, no embedded credentials. Private /
/// loopback hosts are refused here — a payment processor endpoint is a
/// public service, never localhost.
pub(crate) fn validate_payment_endpoint(endpoint: Option<&str>, kind: &str) -> UnifiedResult<Url> {
    let Some(raw) = endpoint else {
        return Err(UnifiedError::bad_request(format!(
            "Provider \"{kind}\" requires endpoint (the processor / gateway API base URL)"
        )));
    };
    let url = Url::parse(raw.trim())
        .map_err(|_| UnifiedError::bad_request(format!("Invalid endpoint URL \"{raw}\"")))?;
    if url.scheme() != "https" {
        return Err(UnifiedError::bad_request(
            "Payment endpoint URL must use https",
        ));
    }
    let host = url.host_str().unwrap_or("");
    if host.is_empty() {
        return Err(UnifiedError::bad_request(
            "Endpoint URL must include a host",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "Endpoint URL must not embed credentials; use the credential fields",
        ));
    }
    if host == "localhost" || host.ends_with(".localhost") || host == "127.0.0.1" || host == "[::1]" {
        return Err(UnifiedError::bad_request(
            "Payment endpoint must not point at localhost",
        ));
    }
    Ok(url)
}


/// Validate a Mastercard gateway host: it must be a `*.mastercard.com`
/// host (the merchant gateway host supplied by Mastercard).
pub(crate) fn validate_mastercard_endpoint(endpoint: &str) -> UnifiedResult<()> {
    let url = validate_payment_endpoint(Some(endpoint), "mastercard")?;
    let host = url.host_str().unwrap_or("");
    if host != "mastercard.com" && !host.ends_with(".mastercard.com") {
        return Err(UnifiedError::bad_request(format!(
            "Mastercard gateway host \"{host}\" must be a *.mastercard.com host"
        )));
    }
    Ok(())
}


/// Validate a custom gateway config: known auth type, paths that start
/// with `/`, and no path that tries to escape the base URL.
pub(crate) fn validate_custom_config(config: &CustomGatewayConfig) -> UnifiedResult<()> {
    match config.auth_type.as_str() {
        "none" | "bearer" | "basic" | "api_key_header" | "api_key_query" | "hmac_sha256" => {}
        other => {
            return Err(UnifiedError::bad_request(format!(
                "Unknown custom auth type \"{other}\" (expected none, bearer, basic, api_key_header, api_key_query, or hmac_sha256)"
            )));
        }
    }
    let mut paths: Vec<&str> = vec![config.charge_path.as_str()];
    for path in [
        &config.authorize_path,
        &config.capture_path,
        &config.refund_path,
        &config.void_path,
        &config.subscription_path,
        &config.payout_path,
        &config.test_path,
    ] {
        if let Some(path) = path {
            paths.push(path.as_str());
        }
    }
    for path in paths {
        if !path.starts_with('/') {
            return Err(UnifiedError::bad_request(format!(
                "Custom endpoint path \"{path}\" must start with '/'"
            )));
        }
        if path.contains("..") {
            return Err(UnifiedError::bad_request(format!(
                "Custom endpoint path \"{path}\" must not contain '..'"
            )));
        }
    }
    if config.supports_subscriptions && config.subscription_path.is_none() {
        return Err(UnifiedError::bad_request(
            "Custom provider with supports_subscriptions needs subscription_path",
        ));
    }
    if config.supports_payouts && config.payout_path.is_none() {
        return Err(UnifiedError::bad_request(
            "Custom provider with supports_payouts needs payout_path",
        ));
    }
    Ok(())
}


/// Required credential / field set per kind. A bad connection is
/// rejected at creation time instead of failing its first API call.
pub(crate) fn validate_connection(input: &CreateConnectionRequest) -> UnifiedResult<PaymentProviderKind> {
    let kind = PaymentProviderKind::from_str(&input.kind).ok_or_else(|| {
        UnifiedError::bad_request(format!("Unknown payment provider kind \"{}\"", input.kind))
    })?;
    if input.name.trim().is_empty() {
        return Err(UnifiedError::bad_request("Connection name is required"));
    }
    let mode = match &input.mode {
        None => GatewayMode::Sandbox,
        Some(mode) => GatewayMode::from_str(mode).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown gateway mode \"{mode}\" (expected sandbox or live)"
            ))
        })?,
    };
    let _ = mode;
    let creds = &input.credentials;
    let has = |value: &Option<String>| {
        value.as_deref().map(|v| !v.trim().is_empty()).unwrap_or(false)
    };
    let missing = |field: &str| {
        UnifiedError::bad_request(format!(
            "Provider \"{}\" requires credential field \"{field}\"",
            kind.as_str()
        ))
    };
    match kind {
        PaymentProviderKind::Xendit => {
            if !has(&creds.api_key) {
                return Err(missing("api_key"));
            }
            if !has(&creds.callback_token) {
                return Err(missing("callback_token"));
            }
        }
        PaymentProviderKind::HyperPay => {
            if !has(&creds.access_token) {
                return Err(missing("access_token"));
            }
            if !has(&creds.merchant_id) {
                return Err(missing("merchant_id"));
            }
        }
        PaymentProviderKind::TwoCheckout => {
            if !has(&creds.merchant_id) {
                return Err(missing("merchant_id"));
            }
            if !has(&creds.secret_word) && !has(&creds.api_key) {
                return Err(UnifiedError::bad_request(
                    "Provider \"2checkout\" requires secret_word (or api_key)",
                ));
            }
        }
        PaymentProviderKind::Card => {
            if !has(&creds.api_key) {
                return Err(missing("api_key"));
            }
            validate_payment_endpoint(input.endpoint.as_deref(), "card")?;
        }
        PaymentProviderKind::Mastercard => {
            if !has(&creds.merchant_id) {
                return Err(missing("merchant_id"));
            }
            if !has(&creds.secret_key) {
                return Err(missing("secret_key"));
            }
            let endpoint = input.endpoint.as_deref().ok_or_else(|| {
                UnifiedError::bad_request(
                    "Provider \"mastercard\" requires endpoint (the merchant gateway host)",
                )
            })?;
            validate_mastercard_endpoint(endpoint)?;
        }
        PaymentProviderKind::GooglePay | PaymentProviderKind::ApplePay => {
            if input.delegate_connection_id.is_none() {
                return Err(UnifiedError::bad_request(format!(
                    "Provider \"{}\" requires delegate_connection_id (the gateway connection that performs the charge)",
                    kind.as_str()
                )));
            }
            let delegate_id = input.delegate_connection_id.unwrap();
            let Some(delegate) = connections_store().get(&delegate_id) else {
                return Err(UnifiedError::bad_request(format!(
                    "Delegate connection \"{delegate_id}\" does not exist"
                )));
            };
            if delegate.kind.needs_delegate() {
                return Err(UnifiedError::bad_request(
                    "A wallet provider cannot delegate to another wallet provider",
                ));
            }
            if !delegate.enabled {
                return Err(UnifiedError::bad_request(
                    "Delegate connection is disabled",
                ));
            }
        }
        PaymentProviderKind::PayPal => {
            if !has(&creds.client_id) {
                return Err(missing("client_id"));
            }
            if !has(&creds.client_secret) {
                return Err(missing("client_secret"));
            }
            if !has(&creds.webhook_id) {
                return Err(missing("webhook_id"));
            }
        }
        PaymentProviderKind::Stripe => {
            if !has(&creds.secret_key) {
                return Err(missing("secret_key"));
            }
            if !has(&creds.webhook_secret) {
                return Err(missing("webhook_secret"));
            }
        }
        PaymentProviderKind::Midtrans => {
            if !has(&creds.server_key) {
                return Err(missing("server_key"));
            }
        }
        PaymentProviderKind::BankTransfer => {
            // No credentials and no API endpoint: bank accounts are
            // configured in the bank-transfer module, not here.
            if let Some(endpoint) = input.endpoint.as_deref() {
                if !endpoint.trim().is_empty() {
                    return Err(UnifiedError::bad_request(
                        "Provider \"bank_transfer\" does not use an API endpoint; bank accounts are configured in the bank-transfer module",
                    ));
                }
            }
        }
        PaymentProviderKind::Custom => {
            validate_payment_endpoint(input.endpoint.as_deref(), "custom")?;
            let Some(config) = &input.custom_config else {
                return Err(UnifiedError::bad_request(
                    "Provider \"custom\" requires custom_config (auth type and endpoint paths)",
                ));
            };
            validate_custom_config(config)?;
        }
    }
    if let Some(endpoint) = input.endpoint.as_deref() {
        if kind.has_fixed_hosts() {
            let url = Url::parse(endpoint.trim()).map_err(|_| {
                UnifiedError::bad_request(format!("Invalid endpoint URL \"{endpoint}\""))
            })?;
            let host = url.host_str().unwrap_or("");
            if !kind.allowed_hosts().contains(&host) {
                return Err(UnifiedError::bad_request(format!(
                    "Endpoint host \"{host}\" is not an allowed {} API host",
                    kind.as_str()
                )));
            }
        }
    }
    Ok(kind)
}
