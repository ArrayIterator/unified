//! panels module - DirectAdmin adapter.

use unified_core::error::UnifiedResult;
use async_trait::async_trait;
use serde_json::Value;
use url::Url;

use super::adapter::PanelAdapter;
use super::adapter::build_client;
use super::adapter::check_panel_host;
use super::adapter::extract_array;
use super::adapter::extract_map_records;
use super::adapter::first_string;
use super::adapter::join_path;
use super::adapter::json_string;
use super::adapter::json_string_owned;
use super::adapter::normalize_status;
use super::adapter::send_text;
use super::handlers_connections::test_connection;
use super::types::PanelClient;
use super::types::PanelConnection;
use super::types::PanelDomain;
use super::types::PanelInfo;
use super::types::PanelKind;
use super::types::PanelProduct;
use super::types::PanelServerAccount;
use super::types::validate_panel_endpoint;

// DirectAdmin adapter.

/// DirectAdmin API (`<endpoint>/CMD_API_*?json=yes`, HTTP basic auth
/// with the user and either the password or a login key).
///
/// Honest scope: users (`CMD_API_SHOW_ALL_USERS`, exposed as clients
/// and as server accounts), domains (`CMD_API_SHOW_DOMAINS` per user
/// is not walked in this round; domains come from the user list's
/// `domain` field when present, so installs that do not report a
/// domain per user return no domains), and packages
/// (`CMD_API_PACKAGES_USER`). DirectAdmin exposes no billing services
/// or invoices through this API, so those collections are unsupported.
/// The JSON shapes vary by DirectAdmin version; the parser accepts an
/// array, a `list` array, and a map of objects.
pub struct DirectAdminAdapter {
    pub(crate) connection: PanelConnection,
    pub(crate) client: reqwest::Client,
    pub(crate) base: Url,
}

impl DirectAdminAdapter {
    pub fn new(connection: &PanelConnection) -> UnifiedResult<Self> {
        let endpoint = connection.endpoint.clone().unwrap_or_default();
        let base = validate_panel_endpoint(Some(&endpoint), "directadmin", connection.allow_private_endpoint)?;
        Ok(Self {
            connection: connection.clone(),
            client: build_client(&base, connection.allow_private_endpoint)?,
            base,
        })
    }

    pub(crate) fn request(&self, path: &str) -> UnifiedResult<reqwest::RequestBuilder> {
        let url = join_path(&self.base, path)?;
        check_panel_host(&url, &self.base, "directadmin", self.connection.allow_private_endpoint)?;
        let url = {
            let mut url = url;
            url.query_pairs_mut().append_pair("json", "yes");
            url
        };
        let user = self
            .connection
            .credentials
            .username
            .clone()
            .unwrap_or_default();
        let secret = self
            .connection
            .credentials
            .password
            .clone()
            .or_else(|| self.connection.credentials.api_token.clone())
            .unwrap_or_default();
        Ok(self.client.get(url).basic_auth(user, Some(secret)))
    }

    pub(crate) async fn call(&self, path: &str) -> UnifiedResult<Value> {
        let text = send_text(&self.client, self.request(path)?, "directadmin").await?;
        // DirectAdmin answers JSON when `json=yes` is honoured, and a
        // URL-encoded list otherwise; parse JSON first and fall back to
        // a `key=value&key=value` list of usernames.
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            return Ok(value);
        }
        let mut items: Vec<Value> = Vec::new();
        for token in text.split(|c| c == '&' || c == '\n') {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            // `list=user1&list=user2` or a bare `user1` per line: the
            // value after `=` (or the whole token) is the username.
            let name = token
                .split_once('=')
                .map(|(_, value)| value)
                .unwrap_or(token);
            let decoded: String = url::form_urlencoded::parse(name.as_bytes())
                .map(|(key, _)| key.into_owned())
                .collect();
            let name = if decoded.is_empty() {
                name.to_string()
            } else {
                decoded
            };
            if !name.is_empty() && name != "list" {
                items.push(serde_json::json!({ "username": name }));
            }
        }
        Ok(Value::Array(items))
    }

    pub(crate) async fn users(&self) -> UnifiedResult<Vec<Value>> {
        let body = self.call("/CMD_API_SHOW_ALL_USERS").await?;
        let items = extract_array(&body, Some("list"), &["list", "users"]);
        if !items.is_empty() {
            return Ok(items.into_iter().cloned().collect());
        }
        Ok(extract_map_records(&body, &["list"]))
    }
}

#[async_trait]
impl PanelAdapter for DirectAdminAdapter {
    fn kind(&self) -> PanelKind {
        PanelKind::DirectAdmin
    }

    async fn test_connection(&self) -> UnifiedResult<PanelInfo> {
        let started = std::time::Instant::now();
        self.call("/CMD_API_SHOW_ALL_USERS").await?;
        Ok(PanelInfo {
            kind: "directadmin".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: self.connection.credentials.username.clone(),
            detail: "DirectAdmin CMD_API_SHOW_ALL_USERS succeeded".to_string(),
        })
    }

    async fn list_clients(&self) -> UnifiedResult<Vec<PanelClient>> {
        let users = self.users().await?;
        Ok(users
            .iter()
            .filter_map(|user| {
                let username = first_string(user, &["username", "user", "id"])?;
                Some(PanelClient {
                    id: username.clone(),
                    name: first_string(user, &["name", "fullname"])
                        .unwrap_or_else(|| username.clone()),
                    email: json_string_owned(user, "email"),
                    company: None,
                    status: normalize_status(json_string(user, "status").unwrap_or("active")),
                    created_at: json_string_owned(user, "created"),
                })
            })
            .collect())
    }

    async fn list_products(&self) -> UnifiedResult<Vec<PanelProduct>> {
        let body = self.call("/CMD_API_PACKAGES_USER").await?;
        let items = extract_array(&body, Some("list"), &["list", "packages"]);
        let mut products: Vec<PanelProduct> = items
            .iter()
            .filter_map(|item| {
                let name = first_string(item, &["name", "package"])?;
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
            .collect();
        if products.is_empty() {
            // Map-shaped package list: keys are package names.
            for record in extract_map_records(&body, &["list"]) {
                if let Some(name) = first_string(&record, &["name", "package", "id"]) {
                    products.push(PanelProduct {
                        id: name.clone(),
                        name,
                        description: None,
                        price: None,
                        currency: None,
                        billing_cycle: None,
                        status: "active".to_string(),
                    });
                }
            }
        }
        Ok(products)
    }

    async fn list_domains(&self) -> UnifiedResult<Vec<PanelDomain>> {
        let users = self.users().await?;
        Ok(users
            .iter()
            .filter_map(|user| {
                let domain = first_string(user, &["domain"])?;
                let username = first_string(user, &["username", "user", "id"]);
                Some(PanelDomain {
                    id: domain.clone(),
                    name: domain,
                    client_id: username,
                    status: "active".to_string(),
                    expires_at: None,
                })
            })
            .collect())
    }

    async fn list_server_accounts(&self) -> UnifiedResult<Vec<PanelServerAccount>> {
        let users = self.users().await?;
        Ok(users
            .iter()
            .filter_map(|user| {
                let username = first_string(user, &["username", "user", "id"])?;
                Some(PanelServerAccount {
                    id: username.clone(),
                    username,
                    domain: first_string(user, &["domain"]),
                    plan: first_string(user, &["package", "plan"]),
                    status: normalize_status(json_string(user, "status").unwrap_or("active")),
                    ip: first_string(user, &["ip"]),
                })
            })
            .collect())
    }
}
