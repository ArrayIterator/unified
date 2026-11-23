//! panels module - Plesk adapter.

use unified_core::error::UnifiedResult;
use async_trait::async_trait;
use serde_json::Value;
use url::Url;

use super::adapter::PanelAdapter;
use super::adapter::api_key_header;
use super::adapter::build_client;
use super::adapter::check_panel_host;
use super::adapter::display_name;
use super::adapter::extract_array;
use super::adapter::first_string;
use super::adapter::join_path;
use super::adapter::json_string;
use super::adapter::json_string_owned;
use super::adapter::normalize_status;
use super::adapter::send_json;
use super::handlers_connections::test_connection;
use super::types::PanelClient;
use super::types::PanelConnection;
use super::types::PanelDomain;
use super::types::PanelInfo;
use super::types::PanelKind;
use super::types::PanelProduct;
use super::types::PanelService;
use super::types::validate_panel_endpoint;

// ---------------------------------------------------------------------------
// Plesk adapter.
// ---------------------------------------------------------------------------

/// Plesk JSON API (`<endpoint>/api/v2/<resource>`, header
/// `X-API-Key: <key>`).
///
/// Honest scope: clients, subscriptions (exposed as services),
/// domains, and service plans (exposed as products). Plesk exposes
/// no billing invoices through this API, so invoices are unsupported.
pub struct PleskAdapter {
    pub(crate) connection: PanelConnection,
    pub(crate) client: reqwest::Client,
    pub(crate) base: Url,
}

impl PleskAdapter {
    pub fn new(connection: &PanelConnection) -> UnifiedResult<Self> {
        let endpoint = connection.endpoint.clone().unwrap_or_default();
        let base = validate_panel_endpoint(Some(&endpoint), "plesk", connection.allow_private_endpoint)?;
        Ok(Self {
            connection: connection.clone(),
            client: build_client(&base, connection.allow_private_endpoint)?,
            base,
        })
    }

    pub(crate) fn key(&self) -> String {
        self.connection
            .credentials
            .api_key
            .clone()
            .or_else(|| self.connection.credentials.api_token.clone())
            .unwrap_or_default()
    }

    pub(crate) async fn call(&self, path: &str) -> UnifiedResult<Value> {
        let url = join_path(&self.base, path)?;
        check_panel_host(&url, &self.base, "plesk", self.connection.allow_private_endpoint)?;
        let headers = api_key_header("X-API-Key", &self.key())?;
        send_json(&self.client, self.client.get(url).headers(headers), "plesk").await
    }
}

#[async_trait]
impl PanelAdapter for PleskAdapter {
    fn kind(&self) -> PanelKind {
        PanelKind::Plesk
    }

    async fn test_connection(&self) -> UnifiedResult<PanelInfo> {
        let started = std::time::Instant::now();
        self.call("/api/v2/clients").await?;
        Ok(PanelInfo {
            kind: "plesk".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: None,
            detail: "Plesk /api/v2/clients succeeded".to_string(),
        })
    }

    async fn list_clients(&self) -> UnifiedResult<Vec<PanelClient>> {
        let body = self.call("/api/v2/clients").await?;
        let items = extract_array(&body, None, &[]);
        Ok(items
            .iter()
            .filter_map(|item| {
                let id = first_string(item, &["id", "guid"])?;
                Some(PanelClient {
                    id,
                    name: display_name(item),
                    email: json_string_owned(item, "email"),
                    company: json_string_owned(item, "company"),
                    status: normalize_status(json_string(item, "status").unwrap_or("active")),
                    created_at: json_string_owned(item, "created"),
                })
            })
            .collect())
    }

    async fn list_products(&self) -> UnifiedResult<Vec<PanelProduct>> {
        let body = self.call("/api/v2/service-plans").await?;
        let items = extract_array(&body, None, &[]);
        Ok(items
            .iter()
            .filter_map(|item| {
                let id = first_string(item, &["id", "guid"])?;
                Some(PanelProduct {
                    id,
                    name: first_string(item, &["name"]).unwrap_or_else(|| "Unknown".to_string()),
                    description: json_string_owned(item, "description"),
                    price: None,
                    currency: None,
                    billing_cycle: None,
                    status: normalize_status(json_string(item, "status").unwrap_or("active")),
                })
            })
            .collect())
    }

    async fn list_services(&self) -> UnifiedResult<Vec<PanelService>> {
        let body = self.call("/api/v2/subscriptions").await?;
        let items = extract_array(&body, None, &[]);
        Ok(items
            .iter()
            .filter_map(|item| {
                let id = first_string(item, &["id", "guid"])?;
                Some(PanelService {
                    id,
                    name: first_string(item, &["name"]).unwrap_or_else(|| "Unknown".to_string()),
                    client_id: first_string(item, &["ownerId", "client_id", "owner_id"]),
                    product_id: first_string(item, &["planId", "plan_id", "service_plan_id"]),
                    domain: first_string(item, &["name", "domain"]),
                    status: normalize_status(json_string(item, "status").unwrap_or("active")),
                })
            })
            .collect())
    }

    async fn list_domains(&self) -> UnifiedResult<Vec<PanelDomain>> {
        let body = self.call("/api/v2/domains").await?;
        let items = extract_array(&body, None, &[]);
        Ok(items
            .iter()
            .filter_map(|item| {
                let id = first_string(item, &["id", "guid"])?;
                Some(PanelDomain {
                    id,
                    name: first_string(item, &["name", "asciiName"])
                        .unwrap_or_else(|| "Unknown".to_string()),
                    client_id: first_string(item, &["ownerId", "client_id", "owner_id"]),
                    status: normalize_status(json_string(item, "status").unwrap_or("active")),
                    expires_at: None,
                })
            })
            .collect())
    }
}
