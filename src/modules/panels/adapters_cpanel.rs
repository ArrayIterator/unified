//! panels module - cPanel / WHM adapter.

use unified_core::error::{UnifiedError, UnifiedResult};
use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde_json::Value;
use url::Url;

use super::adapter::PanelAdapter;
use super::adapter::build_client;
use super::adapter::check_panel_host;
use super::adapter::extract_array;
use super::adapter::first_string;
use super::adapter::join_path;
use super::adapter::json_string;
use super::adapter::normalize_status;
use super::adapter::send_json;
use super::adapter::value_to_string;
use super::handlers_connections::test_connection;
use super::types::PanelConnection;
use super::types::PanelDomain;
use super::types::PanelInfo;
use super::types::PanelKind;
use super::types::PanelProduct;
use super::types::PanelServerAccount;
use super::types::validate_panel_endpoint;

// ---------------------------------------------------------------------------
// cPanel / WHM adapter.
// ---------------------------------------------------------------------------

/// WHM JSON API (`<endpoint>/json-api/<call>?api.version=1`, auth
/// header `WHM <user>:<token>`).
///
/// Honest scope: server accounts (`listaccts`), packages
/// (`listpkgs`, exposed as products), and the domains reported by
/// `listaccts` (primary domain plus addon domains). WHM exposes no
/// billing clients, services, or invoices, so those collections are
/// unsupported.
pub struct CpanelAdapter {
    pub(crate) connection: PanelConnection,
    pub(crate) client: reqwest::Client,
    pub(crate) base: Url,
}

impl CpanelAdapter {
    pub fn new(connection: &PanelConnection) -> UnifiedResult<Self> {
        let endpoint = connection.endpoint.clone().unwrap_or_default();
        let base = validate_panel_endpoint(Some(&endpoint), "cpanel", connection.allow_private_endpoint)?;
        Ok(Self {
            connection: connection.clone(),
            client: build_client(&base, connection.allow_private_endpoint)?,
            base,
        })
    }

    pub(crate) fn auth_headers(&self) -> UnifiedResult<HeaderMap> {
        let user = self
            .connection
            .credentials
            .username
            .clone()
            .unwrap_or_default();
        let token = self
            .connection
            .credentials
            .api_token
            .clone()
            .unwrap_or_default();
        let value = HeaderValue::from_str(&format!("whm {user}:{token}")).map_err(|_| {
            UnifiedError::bad_request(
                "Stored credential contains characters invalid in an HTTP header",
            )
        })?;
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, value);
        Ok(headers)
    }

    pub(crate) async fn call(&self, call: &str) -> UnifiedResult<Value> {
        let url = join_path(&self.base, &format!("/json-api/{call}"))?;
        check_panel_host(&url, &self.base, "cpanel", self.connection.allow_private_endpoint)?;
        let url = {
            let mut url = url;
            url.query_pairs_mut().append_pair("api.version", "1");
            url
        };
        send_json(
            &self.client,
            self.client.get(url).headers(self.auth_headers()?),
            "cpanel",
        )
        .await
    }

    pub(crate) async fn accounts(&self) -> UnifiedResult<Vec<Value>> {
        let body = self.call("listaccts").await?;
        let items = extract_array(&body, None, &["data.acct", "acct"]);
        Ok(items.into_iter().cloned().collect())
    }
}

#[async_trait]
impl PanelAdapter for CpanelAdapter {
    fn kind(&self) -> PanelKind {
        PanelKind::Cpanel
    }

    async fn test_connection(&self) -> UnifiedResult<PanelInfo> {
        let started = std::time::Instant::now();
        self.call("listaccts").await?;
        Ok(PanelInfo {
            kind: "cpanel".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: self.connection.credentials.username.clone(),
            detail: "WHM listaccts succeeded".to_string(),
        })
    }

    async fn list_products(&self) -> UnifiedResult<Vec<PanelProduct>> {
        let body = self.call("listpkgs").await?;
        let items = extract_array(&body, None, &["data.pkg", "pkg"]);
        Ok(items
            .iter()
            .filter_map(|item| {
                let name = first_string(item, &["name", "pkg"])?;
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
        let accounts = self.accounts().await?;
        let mut domains = Vec::new();
        for account in &accounts {
            let owner = first_string(account, &["user"]);
            let mut names: Vec<String> = Vec::new();
            if let Some(domain) = first_string(account, &["domain"]) {
                names.push(domain);
            }
            if let Some(Value::Array(addons)) = account.get("addon_domains") {
                for addon in addons {
                    if let Some(name) = value_to_string(addon) {
                        names.push(name);
                    }
                }
            }
            for name in names {
                domains.push(PanelDomain {
                    id: name.clone(),
                    name,
                    client_id: owner.clone(),
                    status: normalize_status(json_string(account, "suspended").map_or(
                        "active",
                        |suspended| {
                            if suspended == "1" {
                                "suspended"
                            } else {
                                "active"
                            }
                        },
                    )),
                    expires_at: None,
                });
            }
        }
        Ok(domains)
    }

    async fn list_server_accounts(&self) -> UnifiedResult<Vec<PanelServerAccount>> {
        let accounts = self.accounts().await?;
        Ok(accounts
            .iter()
            .filter_map(|account| {
                let username = first_string(account, &["user"])?;
                Some(PanelServerAccount {
                    id: username.clone(),
                    username,
                    domain: first_string(account, &["domain"]),
                    plan: first_string(account, &["plan"]),
                    status: if json_string(account, "suspended") == Some("1") {
                        "suspended".to_string()
                    } else {
                        "active".to_string()
                    },
                    ip: first_string(account, &["ip"]),
                })
            })
            .collect())
    }
}
