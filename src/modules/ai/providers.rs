//! ai module - Provider registry, HTTP transport and base URL rules.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::provider_registry::valid_provider_id;
use async_trait::async_trait;
use reqwest::StatusCode;
use reqwest::header::RETRY_AFTER;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

use super::adapters::AnthropicAdapter;
use super::adapters::OpenAiCompatibleAdapter;
use super::constants::MAX_RETRY_AFTER;
use super::constants::PROVIDER_CONNECT_TIMEOUT_SECS;
use super::constants::PROVIDER_TIMEOUT_SECS;
use super::constants::USER_AGENT;
use super::gating::is_forbidden_mcp_ip;
use super::handlers_drafts::complete;
use super::helpers::ok;
use super::types::AiProviderConnection;
use super::types::AiProviderKind;

// ---------------------------------------------------------------------------
// Provider registry: the plug-in point for adapters.
// ---------------------------------------------------------------------------

/// One completion call to a provider adapter.
#[derive(Debug, Clone)]
pub struct CompletionCall {
    pub prompt: String,
    pub system_prompt: Option<String>,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
}

/// Raw provider answer, before it is stored as a draft.
#[derive(Debug, Clone)]
pub struct RawCompletion {
    pub text: String,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub model: String,
}

/// Result of a live connection test.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderTestResult {
    pub provider_id: Uuid,
    pub kind: String,
    pub reachable: bool,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_count: Option<usize>,
    pub detail: String,
}

/// The interface every AI provider adapter implements. A new provider
/// plugs in by implementing this trait and registering a factory (see
/// `AiProviderRegistry::register_factory`); no other code changes.
#[async_trait]
pub trait AiProviderAdapter: Send + Sync {
    fn kind(&self) -> AiProviderKind;
    fn model(&self) -> &str;
    async fn complete(&self, call: &CompletionCall) -> UnifiedResult<RawCompletion>;
    async fn test_connection(&self) -> UnifiedResult<ProviderTestResult>;
}

/// Factory that builds an adapter for one stored connection plus its
/// decrypted key (`None` for a keyless local endpoint).
pub type AiProviderFactory = Arc<
    dyn Fn(&AiProviderConnection, Option<String>) -> UnifiedResult<Arc<dyn AiProviderAdapter>>
        + Send
        + Sync,
>;

pub struct AiProviderRegistry {
    pub(crate) factories: RwLock<HashMap<AiProviderKind, AiProviderFactory>>,
    /// Factories keyed by registry definition id (C12, task 269):
    /// the extension point for a provider an admin registered
    /// through the provider registry, which has no enum variant.
    /// The factory receives the connection plus its decrypted key
    /// and decides how to build its adapter.
    pub(crate) factories_by_id: RwLock<HashMap<String, AiProviderFactory>>,
}

impl AiProviderRegistry {
    pub(crate) fn new() -> Self {
        Self {
            factories: RwLock::new(HashMap::new()),
            factories_by_id: RwLock::new(HashMap::new()),
        }
    }

    /// Register (or replace) the factory for a kind. This is how a
    /// new provider plugs in: call it once at startup, or from an
    /// extension, instead of editing this module.
    pub fn register_factory(&self, kind: AiProviderKind, factory: AiProviderFactory) {
        self.factories
            .write()
            .expect("ai provider registry lock poisoned")
            .insert(kind, factory);
    }

    /// Register (or replace) the factory for a registry definition id
    /// (C12, task 269). This is how a provider added through the
    /// provider registry — no code change, no enum variant — gets an
    /// adapter: register its definition, then register its factory
    /// under the same id, once at startup or from an extension.
    pub fn register_factory_by_id(
        &self,
        id: &str,
        factory: AiProviderFactory,
    ) -> UnifiedResult<()> {
        if !valid_provider_id(id) {
            return Err(UnifiedError::bad_request(format!(
                "Invalid AI provider id "{id}" (expected lowercase letters, digits, '-' or '_')"
            )));
        }
        self.factories_by_id
            .write()
            .expect("ai provider registry lock poisoned")
            .insert(id.to_string(), factory);
        Ok(())
    }

    pub fn build(
        &self,
        connection: &AiProviderConnection,
        api_key: Option<String>,
    ) -> UnifiedResult<Arc<dyn AiProviderAdapter>> {
        let factories = self
            .factories
            .read()
            .expect("ai provider registry lock poisoned");
        let factory = factories.get(&connection.kind).ok_or_else(|| {
            UnifiedError::internal(format!(
                "No AI provider adapter registered for kind \"{}\"",
                connection.kind.as_str()
            ))
        })?;
        factory(connection, api_key)
    }

    /// Build an adapter through a registry definition id (C12, task
    /// 269). The id must have a factory registered with
    /// `register_factory_by_id`; otherwise this is an honest error
    /// naming the id, never a silent fallback to another provider.
    pub fn build_by_id(
        &self,
        id: &str,
        connection: &AiProviderConnection,
        api_key: Option<String>,
    ) -> UnifiedResult<Arc<dyn AiProviderAdapter>> {
        let factories = self
            .factories_by_id
            .read()
            .expect("ai provider registry lock poisoned");
        let factory = factories.get(id).ok_or_else(|| {
            UnifiedError::internal(format!(
                "No AI provider adapter registered for id "{id}""
            ))
        })?;
        factory(connection, api_key)
    }

    /// Every registered provider id: the built-in kinds plus any
    /// definition ids with a factory, sorted. New providers appear
    /// here with no code change.
    pub fn registered_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .registered_kinds()
            .iter()
            .map(|kind| kind.as_str().to_string())
            .collect();
        let by_id = self
            .factories_by_id
            .read()
            .expect("ai provider registry lock poisoned");
        ids.extend(by_id.keys().cloned());
        ids.sort();
        ids.dedup();
        ids
    }

    pub fn registered_kinds(&self) -> Vec<AiProviderKind> {
        let factories = self
            .factories
            .read()
            .expect("ai provider registry lock poisoned");
        let mut kinds: Vec<AiProviderKind> = factories.keys().copied().collect();
        kinds.sort_by_key(|kind| kind.as_str());
        kinds
    }
}

pub(crate) static REGISTRY: std::sync::OnceLock<AiProviderRegistry> = std::sync::OnceLock::new();

/// The shared registry, with the built-in adapters registered on
/// first use.
pub fn ai_provider_registry() -> &'static AiProviderRegistry {
    REGISTRY.get_or_init(|| {
        let registry = AiProviderRegistry::new();
        registry.register_factory(
            AiProviderKind::Anthropic,
            Arc::new(|connection, api_key| {
                Ok(Arc::new(AnthropicAdapter::new(connection, api_key)?)
                    as Arc<dyn AiProviderAdapter>)
            }),
        );
        registry.register_factory(
            AiProviderKind::OpenAi,
            Arc::new(|connection, api_key| {
                Ok(Arc::new(OpenAiCompatibleAdapter::new(connection, api_key)?)
                    as Arc<dyn AiProviderAdapter>)
            }),
        );
        registry.register_factory(
            AiProviderKind::OpenAiCompatible,
            Arc::new(|connection, api_key| {
                Ok(Arc::new(OpenAiCompatibleAdapter::new(connection, api_key)?)
                    as Arc<dyn AiProviderAdapter>)
            }),
        );
        registry.register_factory(
            AiProviderKind::Local,
            Arc::new(|connection, api_key| {
                Ok(Arc::new(OpenAiCompatibleAdapter::new(connection, api_key)?)
                    as Arc<dyn AiProviderAdapter>)
            }),
        );
        registry
    })
}

// ---------------------------------------------------------------------------
// Shared HTTP helpers.
// ---------------------------------------------------------------------------

pub(crate) fn build_client() -> UnifiedResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(PROVIDER_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(PROVIDER_CONNECT_TIMEOUT_SECS))
        // No redirects: a provider that redirects to an internal
        // address would otherwise receive the API key meant for the
        // validated base URL. A redirect is a provider error, not
        // something to follow.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))
}

/// Parse a `Retry-After` header (seconds only; HTTP dates are
/// ignored), capped so a misbehaving provider cannot stall a caller
/// indefinitely.
pub(crate) fn parse_retry_after(response: &reqwest::Response) -> Option<Duration> {
    let value = response.headers().get(RETRY_AFTER)?.to_str().ok()?;
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(|secs| Duration::from_secs(secs).min(MAX_RETRY_AFTER))
}

/// Map a provider HTTP response to an error. A 429 keeps its status
/// and carries the `Retry-After` delay in metadata; other failures
/// stay generic so provider response bodies (which may contain
/// account data) are never leaked into the client-visible message.
pub(crate) fn http_status_error(
    status: StatusCode,
    provider: &str,
    retry_after: Option<Duration>,
) -> UnifiedError {
    let message = format!("Provider \"{provider}\" request failed with HTTP {status}");
    match status {
        StatusCode::TOO_MANY_REQUESTS => {
            let mut error = UnifiedError::new(message, 429)
                .with_metadata("provider", Value::String(provider.to_string()));
            if let Some(delay) = retry_after {
                error = error.with_metadata("retry_after_secs", Value::from(delay.as_secs()));
            }
            error
        }
        StatusCode::UNAUTHORIZED => UnifiedError::unauthorized(format!(
            "Provider \"{provider}\" rejected the credentials (HTTP 401)"
        )),
        StatusCode::FORBIDDEN => UnifiedError::forbidden(format!(
            "Provider \"{provider}\" refused the request (HTTP 403): the credentials lack permission for this operation"
        )),
        StatusCode::NOT_FOUND => UnifiedError::not_found(format!(
            "Provider \"{provider}\" resource not found (HTTP 404)"
        )),
        _ if status.is_client_error() => UnifiedError::bad_request(message)
            .with_metadata("provider", Value::String(provider.to_string())),
        _ => UnifiedError::internal(message)
            .with_metadata("provider", Value::String(provider.to_string())),
    }
}

/// Send a request and return the parsed JSON body, or a mapped error.
pub(crate) async fn send_json(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    provider: &str,
) -> UnifiedResult<Value> {
    let response = request.send().await.map_err(|error| {
        UnifiedError::internal(format!("Provider \"{provider}\" request failed: {error}"))
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(http_status_error(
            status,
            provider,
            parse_retry_after(&response),
        ));
    }
    response.json::<Value>().await.map_err(|error| {
        UnifiedError::internal(format!(
            "Provider \"{provider}\" returned an unreadable response: {error}"
        ))
    })
}

/// Admin-approved opt-ins for a non-first-party provider
/// endpoint. Both default to `false` (fail closed): a compatible
/// or local endpoint must use https and resolve to public
/// addresses unless an admin explicitly allows otherwise for that
/// connection.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ProviderEndpointOptions {
    pub(crate) allow_private_host: bool,
    pub(crate) allow_http: bool,
}

impl ProviderEndpointOptions {
    pub(crate) fn from_connection(connection: &AiProviderConnection) -> Self {
        Self {
            allow_private_host: connection.allow_private_host,
            allow_http: connection.allow_http,
        }
    }
}

/// Validate a base URL with the default (strict) endpoint options:
/// https only, public hosts only for non-first-party kinds.
pub(crate) fn validate_base_url(base_url: &str, kind: AiProviderKind) -> UnifiedResult<()> {
    validate_base_url_with_options(base_url, kind, ProviderEndpointOptions::default())
}

/// Validate a base URL: http(s), a host, no embedded credentials.
/// Fixed-host providers additionally require an allow-listed host.
/// Non-first-party kinds (compatible / local) have no fixed host,
/// so they get the same discipline as personal MCP endpoints
/// (`validate_mcp_endpoint_url`): https unless the connection
/// explicitly allows http, and the host must resolve to public
/// addresses unless the connection explicitly allows a private
/// host. An unresolvable hostname is refused. Residual risk, as
/// with MCP: DNS rebinding between validation and use; the HTTP
/// client follows no redirects, so a redirect cannot steer the
/// request (and its API key) at a different host.
pub(crate) fn validate_base_url_with_options(
    base_url: &str,
    kind: AiProviderKind,
    options: ProviderEndpointOptions,
) -> UnifiedResult<()> {
    let url = Url::parse(base_url.trim())
        .map_err(|_| UnifiedError::bad_request(format!("Invalid base URL \"{base_url}\"")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UnifiedError::bad_request("Base URL must use http or https"));
    }
    if url.host_str().is_none_or(|host| host.is_empty()) {
        return Err(UnifiedError::bad_request("Base URL must include a host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "Base URL must not embed credentials; use the API key field",
        ));
    }
    let allowed = kind.allowed_hosts();
    if !allowed.is_empty() {
        let host = url.host_str().unwrap_or("");
        if !allowed.contains(&host) {
            return Err(UnifiedError::bad_request(format!(
                "Base URL host \"{host}\" is not an allowed {} API host",
                kind.as_str()
            )));
        }
        return Ok(());
    }
    // Non-first-party kind: no fixed host to allow-list, so the
    // endpoint itself must prove it is a public https service
    // (unless the admin opted the connection into a private host
    // and/or plain http).
    if url.scheme() == "http" && !options.allow_http {
        return Err(UnifiedError::bad_request(
            "Base URL must use https unless the connection explicitly allows http",
        ));
    }
    let host = url.host_str().unwrap_or("");
    let lower = host.to_ascii_lowercase();
    if !options.allow_private_host
        && (lower == "localhost"
            || lower.ends_with(".localhost")
            || lower.ends_with(".local")
            || lower.ends_with(".internal")
            || lower.ends_with(".lan")
            || lower.ends_with(".home")
            || lower.ends_with(".corp")
            || lower.ends_with(".intranet"))
    {
        return Err(UnifiedError::bad_request(
            "Base URL host must be a public host, not a local or internal name, unless the connection explicitly allows a private host",
        ));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_forbidden_mcp_ip(ip) && !options.allow_private_host {
            return Err(UnifiedError::bad_request(
                "Base URL host must be a public IP address, not a private, loopback, or link-local address, unless the connection explicitly allows a private host",
            ));
        }
        return Ok(());
    }
    // Hostname: it must resolve, and every resolved address must be
    // public unless a private host is explicitly allowed.
    let port = url.port_or_known_default().unwrap_or(443);
    let addresses = (host, port).to_socket_addrs().map_err(|_| {
        UnifiedError::bad_request(format!("Base URL host \"{host}\" does not resolve"))
    })?;
    let mut any = false;
    for socket in addresses {
        any = true;
        if is_forbidden_mcp_ip(socket.ip()) && !options.allow_private_host {
            return Err(UnifiedError::bad_request(format!(
                "Base URL host \"{host}\" resolves to a private, loopback, or link-local address; allow a private host explicitly to use it"
            )));
        }
    }
    if !any {
        return Err(UnifiedError::bad_request(format!(
            "Base URL host \"{host}\" does not resolve"
        )));
    }
    Ok(())
}

/// Resolve the effective base URL and model for a connection input:
/// explicit values win, then the kind defaults; compatible and local
/// providers require both explicitly.
pub(crate) fn resolve_base_url_and_model(
    kind: AiProviderKind,
    base_url: &Option<String>,
    model: &Option<String>,
) -> UnifiedResult<(String, String)> {
    resolve_base_url_and_model_with_options(
        kind,
        base_url,
        model,
        ProviderEndpointOptions::default(),
    )
}

/// Same as `resolve_base_url_and_model`, with the admin-approved
/// endpoint options of the connection being created or updated.
pub(crate) fn resolve_base_url_and_model_with_options(
    kind: AiProviderKind,
    base_url: &Option<String>,
    model: &Option<String>,
    options: ProviderEndpointOptions,
) -> UnifiedResult<(String, String)> {
    let base_url = match base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(url) => url.to_string(),
        None => match kind.default_base_url() {
            Some(default) => default.to_string(),
            None => {
                return Err(UnifiedError::bad_request(format!(
                    "Provider \"{}\" requires base_url (the API endpoint)",
                    kind.as_str()
                )));
            }
        },
    };
    validate_base_url_with_options(&base_url, kind, options)?;
    let model = match model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(model) => model.to_string(),
        None => match kind.default_model() {
            Some(default) => default.to_string(),
            None => {
                return Err(UnifiedError::bad_request(format!(
                    "Provider \"{}\" requires model",
                    kind.as_str()
                )));
            }
        },
    };
    Ok((base_url, model))
}

#[cfg(test)]
mod tests {
    use super::ProviderEndpointOptions;
    use super::validate_base_url;
    use super::validate_base_url_with_options;
    use crate::modules::ai::AiProviderKind;

    #[test]
    fn base_url_accepts_allowed_hosts() {
        assert!(validate_base_url("https://api.anthropic.com", AiProviderKind::Anthropic).is_ok());
        assert!(validate_base_url("https://api.openai.com/v1", AiProviderKind::OpenAi).is_ok());
        // Non-first-party kinds accept a public IP-literal host
        // over https (IP literals need no DNS lookup in tests).
        assert!(validate_base_url("https://8.8.8.8/v1", AiProviderKind::Local).is_ok());
        assert!(validate_base_url("https://8.8.8.8/v1", AiProviderKind::OpenAiCompatible).is_ok());
    }

    #[test]
    fn base_url_rejects_bad_urls() {
        assert!(validate_base_url("https://evil.example.com", AiProviderKind::Anthropic).is_err());
        assert!(validate_base_url("ftp://api.openai.com", AiProviderKind::OpenAi).is_err());
        assert!(validate_base_url("https://user:pass@example.com", AiProviderKind::Local).is_err());
        assert!(validate_base_url("not a url", AiProviderKind::Local).is_err());
    }

    #[test]
    fn local_endpoints_require_https_and_public_hosts_by_default() {
        // Plain http to a public host is refused without the
        // explicit allow_http opt-in.
        assert!(validate_base_url("http://8.8.8.8/v1", AiProviderKind::Local).is_err());
        // Private, loopback, and internal names are refused
        // without the explicit allow_private_host opt-in.
        assert!(validate_base_url("https://10.0.0.5/v1", AiProviderKind::Local).is_err());
        assert!(validate_base_url("https://192.168.1.10/v1", AiProviderKind::Local).is_err());
        assert!(validate_base_url("https://127.0.0.1:11434/v1", AiProviderKind::Local).is_err());
        assert!(validate_base_url("https://localhost:11434/v1", AiProviderKind::Local).is_err());
        assert!(validate_base_url("https://model.internal/v1", AiProviderKind::Local).is_err());
        // An unresolvable hostname is refused.
        assert!(
            validate_base_url("https://does-not-resolve.invalid/v1", AiProviderKind::Local)
                .is_err()
        );
    }

    #[test]
    fn local_endpoints_accept_explicit_private_and_http_opt_ins() {
        let private = ProviderEndpointOptions {
            allow_private_host: true,
            allow_http: false,
        };
        assert!(
            validate_base_url_with_options("https://10.0.0.5/v1", AiProviderKind::Local, private)
                .is_ok()
        );
        assert!(
            validate_base_url_with_options(
                "https://127.0.0.1:11434/v1",
                AiProviderKind::Local,
                private
            )
            .is_ok()
        );
        assert!(
            validate_base_url_with_options(
                "https://localhost:11434/v1",
                AiProviderKind::Local,
                private
            )
            .is_ok()
        );
        // Private-host opt-in alone does not allow plain http.
        assert!(
            validate_base_url_with_options("http://10.0.0.5/v1", AiProviderKind::Local, private)
                .is_err()
        );
        let both = ProviderEndpointOptions {
            allow_private_host: true,
            allow_http: true,
        };
        assert!(
            validate_base_url_with_options("http://10.0.0.5/v1", AiProviderKind::Local, both)
                .is_ok()
        );
        // http opt-in alone does not allow a private host.
        let http_only = ProviderEndpointOptions {
            allow_private_host: false,
            allow_http: true,
        };
        assert!(
            validate_base_url_with_options("http://10.0.0.5/v1", AiProviderKind::Local, http_only)
                .is_err()
        );
        assert!(
            validate_base_url_with_options("http://8.8.8.8/v1", AiProviderKind::Local, http_only)
                .is_ok()
        );
    }

    #[test]
    fn build_by_id_dispatches_to_a_registered_definition_factory() {
        // A provider added through the provider registry only: no enum
        // variant, no code change. The factory is invoked by id, an
        // unknown id is an honest error naming the id, and a bad id is
        // rejected at registration time.
        use chrono::Utc;

        let registry = AiProviderRegistry::new();
        let connection = AiProviderConnection {
            id: Uuid::new_v4(),
            name: "Test".to_string(),
            kind: AiProviderKind::Local,
            base_url: "https://127.0.0.1:11434/v1".to_string(),
            model: "test".to_string(),
            api_key_encrypted: None,
            key_fingerprint: None,
            enabled: true,
            rate_limit_per_minute: 60,
            allow_private_host: false,
            allow_http: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        registry
            .register_factory_by_id(
                "acme_ai",
                Arc::new(|_, _| Err(UnifiedError::internal("factory invoked for acme_ai"))),
            )
            .unwrap();
        assert!(registry.registered_ids().contains(&"acme_ai".to_string()));
        let err = registry
            .build_by_id("acme_ai", &connection, None)
            .unwrap_err();
        assert!(err.to_string().contains("factory invoked for acme_ai"));
        let missing = registry
            .build_by_id("no_such", &connection, None)
            .unwrap_err();
        assert!(missing.to_string().contains("no_such"));
        assert!(
            registry
                .register_factory_by_id(
                    "Bad Id",
                    Arc::new(|_, _| Err(UnifiedError::internal("unreachable")))
                )
                .is_err()
        );
    }
}
