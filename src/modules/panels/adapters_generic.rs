//! panels module - Configuration-driven generic adapter.

use unified_core::error::{UnifiedError, UnifiedResult};
use async_trait::async_trait;
use reqwest::header::HeaderMap;
use url::Url;

use super::adapter::PanelAdapter;
use super::adapter::api_key_header;
use super::adapter::bearer_header;
use super::adapter::build_client;
use super::adapter::check_panel_host;
use super::adapter::extract_array;
use super::adapter::first_string;
use super::adapter::join_path;
use super::adapter::json_f64;
use super::adapter::json_string_owned;
use super::adapter::nested_value;
use super::adapter::normalize_status;
use super::adapter::send_json;
use super::adapter::unsupported;
use super::adapter::value_to_string;
use super::handlers_connections::test_connection;
use super::types::GenericPanelConfig;
use super::types::PanelCapabilities;
use super::types::PanelClient;
use super::types::PanelConnection;
use super::types::PanelDomain;
use super::types::PanelInfo;
use super::types::PanelInvoice;
use super::types::PanelKind;
use super::types::PanelProduct;
use super::types::PanelServerAccount;
use super::types::PanelService;
use super::types::RemoteRecord;
use super::types::generic_capabilities;
use super::types::validate_generic_config;
use super::types::validate_panel_endpoint;

// Generic adapter.

/// Configuration-driven adapter for any panel that answers JSON over
/// HTTP: the connection's `generic_config` supplies the auth type, a
/// per-collection endpoint path, and the response field mapping.
/// Only collections with a configured endpoint are supported, and
/// only the configured base host is ever contacted. Credentials are
/// sent as headers only, never in the URL query string (query
/// strings land in access logs and proxies); `api_key_query` is a
/// legacy alias that sends the key in the API-key header.
pub struct GenericAdapter {
    pub(crate) connection: PanelConnection,
    pub(crate) client: reqwest::Client,
    pub(crate) base: Url,
    pub(crate) config: GenericPanelConfig,
}

impl GenericAdapter {
    pub fn new(connection: &PanelConnection) -> UnifiedResult<Self> {
        let endpoint = connection.endpoint.clone().unwrap_or_default();
        let base = validate_panel_endpoint(Some(&endpoint), "generic", connection.allow_private_endpoint)?;
        let config = connection.generic_config.clone().ok_or_else(|| {
            UnifiedError::bad_request("Panel \"generic\" requires generic_config")
        })?;
        validate_generic_config(&config)?;
        Ok(Self {
            connection: connection.clone(),
            client: build_client(&base, connection.allow_private_endpoint)?,
            base,
            config,
        })
    }

    pub(crate) fn auth_headers(&self) -> UnifiedResult<HeaderMap> {
        let mut headers = HeaderMap::new();
        match self.config.auth_type() {
            "bearer" => {
                let token = self
                    .connection
                    .credentials
                    .api_token
                    .clone()
                    .or_else(|| self.connection.credentials.api_key.clone())
                    .unwrap_or_default();
                headers.extend(bearer_header(&token)?);
            }
            // `api_key_header`, and `api_key_query` as a legacy
            // alias: the key goes in a header either way. It is
            // never put in the URL query string, where it would
            // land in access logs, error reports, and proxies.
            "api_key_header" | "api_key_query" => {
                let name = self
                    .config
                    .api_key_header
                    .clone()
                    .unwrap_or_else(|| "X-API-Key".to_string());
                let value = self
                    .connection
                    .credentials
                    .api_key
                    .clone()
                    .or_else(|| self.connection.credentials.api_token.clone())
                    .unwrap_or_default();
                headers.extend(api_key_header(&name, &value)?);
            }
            _ => {}
        }
        Ok(headers)
    }

    pub(crate) async fn fetch(&self, collection: &str) -> UnifiedResult<Vec<RemoteRecord>> {
        let path = self
            .config
            .endpoints
            .get(collection)
            .ok_or_else(|| unsupported(PanelKind::Generic, collection))?;
        let url = join_path(&self.base, path)?;
        check_panel_host(&url, &self.base, "generic", self.connection.allow_private_endpoint)?;
        // No credential is ever appended to the URL: `api_key_query`
        // sends the key as a header (see `auth_headers`). Query
        // strings are logged by servers and proxies.
        let mut request = self.client.get(url).headers(self.auth_headers()?);
        if self.config.auth_type() == "basic" {
            request = request.basic_auth(
                self.connection
                    .credentials
                    .username
                    .clone()
                    .unwrap_or_default(),
                self.connection.credentials.password.clone(),
            );
        }
        let body = send_json(&self.client, request, "generic").await?;
        let mapping = self
            .config
            .field_mapping
            .get(collection)
            .cloned()
            .unwrap_or_default();
        let items = extract_array(&body, self.config.list_field.as_deref(), &[collection]);
        let mut records = Vec::new();
        for item in items {
            let Some(id_value) = nested_value(item, mapping.id_field()) else {
                continue;
            };
            let Some(remote_id) = value_to_string(id_value) else {
                continue;
            };
            if remote_id.trim().is_empty() {
                continue;
            }
            let name = nested_value(item, mapping.name_field())
                .and_then(value_to_string)
                .unwrap_or_else(|| remote_id.clone());
            let status = nested_value(item, mapping.status_field())
                .and_then(value_to_string)
                .map(|status| normalize_status(&status))
                .unwrap_or_else(|| "unknown".to_string());
            records.push(RemoteRecord {
                remote_id,
                name,
                status,
                payload: item.clone(),
            });
        }
        Ok(records)
    }

    pub(crate) fn to_clients(records: Vec<RemoteRecord>) -> Vec<PanelClient> {
        records
            .into_iter()
            .map(|record| PanelClient {
                id: record.remote_id,
                name: record.name,
                email: json_string_owned(&record.payload, "email"),
                company: json_string_owned(&record.payload, "company"),
                status: record.status,
                created_at: json_string_owned(&record.payload, "created_at"),
            })
            .collect()
    }

    pub(crate) fn to_products(records: Vec<RemoteRecord>) -> Vec<PanelProduct> {
        records
            .into_iter()
            .map(|record| PanelProduct {
                id: record.remote_id,
                name: record.name,
                description: json_string_owned(&record.payload, "description"),
                price: json_f64(&record.payload, "price"),
                currency: json_string_owned(&record.payload, "currency"),
                billing_cycle: json_string_owned(&record.payload, "billing_cycle"),
                status: record.status,
            })
            .collect()
    }

    pub(crate) fn to_services(records: Vec<RemoteRecord>) -> Vec<PanelService> {
        records
            .into_iter()
            .map(|record| PanelService {
                id: record.remote_id,
                name: record.name,
                client_id: first_string(&record.payload, &["client_id", "clientId"]),
                product_id: first_string(&record.payload, &["product_id", "productId"]),
                domain: json_string_owned(&record.payload, "domain"),
                status: record.status,
            })
            .collect()
    }

    pub(crate) fn to_domains(records: Vec<RemoteRecord>) -> Vec<PanelDomain> {
        records
            .into_iter()
            .map(|record| PanelDomain {
                id: record.remote_id,
                name: record.name,
                client_id: first_string(&record.payload, &["client_id", "clientId"]),
                status: record.status,
                expires_at: json_string_owned(&record.payload, "expires_at"),
            })
            .collect()
    }

    pub(crate) fn to_invoices(records: Vec<RemoteRecord>) -> Vec<PanelInvoice> {
        records
            .into_iter()
            .map(|record| PanelInvoice {
                id: record.remote_id,
                client_id: first_string(&record.payload, &["client_id", "clientId"]),
                total: json_f64(&record.payload, "total"),
                currency: json_string_owned(&record.payload, "currency"),
                status: record.status,
                due_date: json_string_owned(&record.payload, "due_date"),
            })
            .collect()
    }

    pub(crate) fn to_server_accounts(records: Vec<RemoteRecord>) -> Vec<PanelServerAccount> {
        records
            .into_iter()
            .map(|record| PanelServerAccount {
                id: record.remote_id.clone(),
                username: first_string(&record.payload, &["username", "user"])
                    .unwrap_or_else(|| record.remote_id.clone()),
                domain: json_string_owned(&record.payload, "domain"),
                plan: first_string(&record.payload, &["plan", "package"]),
                status: record.status,
                ip: json_string_owned(&record.payload, "ip"),
            })
            .collect()
    }
}

#[async_trait]
impl PanelAdapter for GenericAdapter {
    fn kind(&self) -> PanelKind {
        PanelKind::Generic
    }

    fn capabilities(&self) -> PanelCapabilities {
        generic_capabilities(&self.config)
    }

    async fn test_connection(&self) -> UnifiedResult<PanelInfo> {
        let started = std::time::Instant::now();
        let Some(collection) = self
            .capabilities()
            .supported_collections()
            .into_iter()
            .next()
        else {
            return Err(UnifiedError::bad_request(
                "Generic panel has no configured endpoints to test against",
            ));
        };
        self.fetch(collection).await?;
        Ok(PanelInfo {
            kind: "generic".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: None,
            detail: format!("Generic panel endpoint for \"{collection}\" succeeded"),
        })
    }

    async fn list_clients(&self) -> UnifiedResult<Vec<PanelClient>> {
        Ok(Self::to_clients(self.fetch("clients").await?))
    }

    async fn list_products(&self) -> UnifiedResult<Vec<PanelProduct>> {
        Ok(Self::to_products(self.fetch("products").await?))
    }

    async fn list_services(&self) -> UnifiedResult<Vec<PanelService>> {
        Ok(Self::to_services(self.fetch("services").await?))
    }

    async fn list_domains(&self) -> UnifiedResult<Vec<PanelDomain>> {
        Ok(Self::to_domains(self.fetch("domains").await?))
    }

    async fn list_invoices(&self) -> UnifiedResult<Vec<PanelInvoice>> {
        Ok(Self::to_invoices(self.fetch("invoices").await?))
    }

    async fn list_server_accounts(&self) -> UnifiedResult<Vec<PanelServerAccount>> {
        Ok(Self::to_server_accounts(
            self.fetch("server_accounts").await?,
        ))
    }
}
