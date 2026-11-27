//! panels module - CyberPanel adapter.

use unified_core::error::UnifiedResult;
use async_trait::async_trait;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::Value;
use url::Url;

use super::adapter::PanelAdapter;
use super::adapter::bearer_header;
use super::adapter::build_client;
use super::adapter::check_panel_host;
use super::adapter::extract_array;
use super::adapter::first_string;
use super::adapter::join_path;
use super::adapter::json_string;
use super::adapter::normalize_status;
use super::adapter::send_json;
use super::handlers_connections::test_connection;
use super::types::PanelConnection;
use super::types::PanelDomain;
use super::types::PanelInfo;
use super::types::PanelKind;
use super::types::PanelProduct;
use super::types::PanelServerAccount;
use super::types::validate_panel_endpoint;

// CyberPanel adapter.

/// CyberPanel JSON API (`<endpoint>/api/<call>`, POST with a JSON
/// body). Authentication is the API token: it is sent as a Bearer
/// header and as the JSON `token` field, because CyberPanel versions
/// differ on which one they read; when no token is configured, the
/// admin username and password are sent as `adminUser` /
/// `adminPass` instead.
///
/// Honest scope: websites (`listWebsites`, exposed as server accounts
/// and as domains) and packages (`listPackages`, exposed as
/// products). CyberPanel exposes no billing clients, services, or
/// invoices through this API, so those collections are unsupported.
pub struct CyberPanelAdapter {
    pub(crate) connection: PanelConnection,
    pub(crate) client: reqwest::Client,
    pub(crate) base: Url,
}

impl CyberPanelAdapter {
    pub fn new(connection: &PanelConnection) -> UnifiedResult<Self> {
        let endpoint = connection.endpoint.clone().unwrap_or_default();
        let base = validate_panel_endpoint(Some(&endpoint), "cyberpanel", connection.allow_private_endpoint)?;
        Ok(Self {
            connection: connection.clone(),
            client: build_client(&base, connection.allow_private_endpoint)?,
            base,
        })
    }

    pub(crate) async fn call(&self, call: &str) -> UnifiedResult<Value> {
        let url = join_path(&self.base, &format!("/api/{call}"))?;
        check_panel_host(&url, &self.base, "cyberpanel", self.connection.allow_private_endpoint)?;
        let mut body = serde_json::Map::new();
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(token) = self.connection.credentials.api_token.clone() {
            if !token.trim().is_empty() {
                headers.extend(bearer_header(&token)?);
                body.insert("token".to_string(), Value::String(token));
            }
        }
        if body.is_empty() {
            if let Some(user) = self.connection.credentials.username.clone() {
                body.insert("adminUser".to_string(), Value::String(user));
            }
            if let Some(password) = self.connection.credentials.password.clone() {
                body.insert("adminPass".to_string(), Value::String(password));
            }
        }
        send_json(
            &self.client,
            self.client
                .post(url)
                .headers(headers)
                .json(&Value::Object(body)),
            "cyberpanel",
        )
        .await
    }

    pub(crate) async fn websites(&self) -> UnifiedResult<Vec<Value>> {
        let body = self.call("listWebsites").await?;
        let items = extract_array(&body, None, &["websites", "data"]);
        Ok(items.into_iter().cloned().collect())
    }
}

#[async_trait]
impl PanelAdapter for CyberPanelAdapter {
    fn kind(&self) -> PanelKind {
        PanelKind::CyberPanel
    }

    async fn test_connection(&self) -> UnifiedResult<PanelInfo> {
        let started = std::time::Instant::now();
        self.call("listPackages").await?;
        Ok(PanelInfo {
            kind: "cyberpanel".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: self.connection.credentials.username.clone(),
            detail: "CyberPanel listPackages succeeded".to_string(),
        })
    }

    async fn list_products(&self) -> UnifiedResult<Vec<PanelProduct>> {
        let body = self.call("listPackages").await?;
        let items = extract_array(&body, None, &["packages", "data"]);
        Ok(items
            .iter()
            .filter_map(|item| {
                let name = first_string(item, &["packageName", "name", "package"])?;
                Some(PanelProduct {
                    id: name.clone(),
                    name,
                    description: None,
                    price: None,
                    currency: None,
                    billing_cycle: None,
                    status: "active".to_string(),
                })
            })
            .collect())
    }

    async fn list_domains(&self) -> UnifiedResult<Vec<PanelDomain>> {
        let websites = self.websites().await?;
        Ok(websites
            .iter()
            .filter_map(|site| {
                let domain = first_string(site, &["domain", "website", "name"])?;
                Some(PanelDomain {
                    id: domain.clone(),
                    name: domain,
                    client_id: first_string(site, &["admin", "owner"]),
                    status: normalize_status(json_string(site, "state").unwrap_or("active")),
                    expires_at: None,
                })
            })
            .collect())
    }

    async fn list_server_accounts(&self) -> UnifiedResult<Vec<PanelServerAccount>> {
        let websites = self.websites().await?;
        Ok(websites
            .iter()
            .filter_map(|site| {
                let domain = first_string(site, &["domain", "website", "name"])?;
                Some(PanelServerAccount {
                    id: domain.clone(),
                    username: first_string(site, &["admin", "owner"])
                        .unwrap_or_else(|| domain.clone()),
                    domain: Some(domain),
                    plan: first_string(site, &["package", "packageName"]),
                    status: normalize_status(json_string(site, "state").unwrap_or("active")),
                    ip: first_string(site, &["ip", "ipAddress"]),
                })
            })
            .collect())
    }
}
