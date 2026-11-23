// providers module -- Provider adapters: Redshift and config-driven custom providers.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use async_trait::async_trait;
use chrono::Utc;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Method;
use serde_json::Value;
use std::time::Instant;
use url::Url;

use super::*;

// ---------------------------------------------------------------------------
// Redshift adapter.
// ---------------------------------------------------------------------------

/// Amazon Redshift (`redshift.<region>.amazonaws.com`), SigV4 signed
/// query API. Clusters list / status only -- read-only scope, no query
/// execution in this round.
pub struct RedshiftAdapter {
    pub(crate) client: reqwest::Client,
    pub(crate) params: SigV4Params,
}

impl RedshiftAdapter {
    pub fn new(connection: &ProviderConnection) -> UnifiedResult<Self> {
        Ok(Self {
            client: build_client()?,
            params: SigV4Params {
                access_key_id: connection
                    .credentials
                    .access_key_id
                    .clone()
                    .unwrap_or_default(),
                secret_access_key: connection
                    .credentials
                    .secret_access_key
                    .clone()
                    .unwrap_or_default(),
                region: connection.region.clone().unwrap_or_default(),
                service: "redshift".to_string(),
            },
        })
    }

    pub(crate) async fn describe_clusters(&self, extra: &[(&str, &str)]) -> UnifiedResult<String> {
        let url = Url::parse(&format!(
            "https://redshift.{}.amazonaws.com/",
            self.params.region
        ))
        .map_err(|_| UnifiedError::internal("Invalid Redshift endpoint"))?;
        let host = url.host_str().unwrap_or("");
        if !host.ends_with(".amazonaws.com") {
            return Err(UnifiedError::bad_request("Invalid Redshift endpoint host"));
        }
        let mut form = "Action=DescribeClusters&Version=2012-12-01".to_string();
        for (key, value) in extra {
            form.push_str(&format!(
                "&{}={}",
                url::form_urlencoded::byte_serialize(key.as_bytes()).collect::<String>(),
                url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>(),
            ));
        }
        let payload = form.as_bytes();
        let now = Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let (authorization, _) = sigv4_authorization(
            &self.params,
            "POST",
            &url,
            &[
                ("content-type", "application/x-www-form-urlencoded; charset=utf-8".to_string()),
                ("host", url.host_str().unwrap_or("").to_string()),
                ("x-amz-date", amz_date.clone()),
            ],
            payload,
            now,
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&authorization).map_err(|_| {
                UnifiedError::internal("Computed Redshift signature is not a valid header value")
            })?,
        );
        headers.insert(
            "x-amz-date",
            HeaderValue::from_str(&amz_date).map_err(|_| {
                UnifiedError::internal("Computed Redshift date is not a valid header value")
            })?,
        );
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded; charset=utf-8"),
        );
        let request = self.client.post(url).headers(headers).body(form);
        send_text(&self.client, request, "redshift").await
    }

    pub(crate) fn map_cluster(block: &str) -> ServerInfo {
        let id = xml_tag_at(block, 0, "ClusterIdentifier").unwrap_or_default();
        let raw_status = xml_tag_at(block, 0, "ClusterStatus").unwrap_or_default();
        let endpoint_pos = block.find("<Endpoint>").unwrap_or(0);
        let address = xml_tag_at(block, endpoint_pos, "Address");
        let nodes = xml_tag_at(block, 0, "NumberOfNodes")
            .and_then(|nodes| nodes.parse::<u32>().ok());
        ServerInfo {
            id: id.clone(),
            name: id,
            item_kind: "cluster".to_string(),
            status: normalize_status(&raw_status).to_string(),
            raw_status: Some(raw_status),
            region: None,
            public_ip: address,
            private_ip: None,
            cpu_cores: nodes,
            memory_mb: None,
            disk_gb: None,
            image: xml_tag_at(block, 0, "NodeType"),
            created_at: xml_tag_at(block, 0, "ClusterCreateTime"),
        }
    }

    pub(crate) fn parse_clusters(xml: &str) -> Vec<ServerInfo> {
        let mut clusters = Vec::new();
        for set in xml_blocks(xml, "Clusters") {
            for item in xml_blocks(&set, "Cluster") {
                if item.contains("<ClusterIdentifier>") {
                    clusters.push(Self::map_cluster(&item));
                }
            }
        }
        // Some responses nest clusters directly under the response root
        // without a `<Clusters>` wrapper.
        if clusters.is_empty() {
            for item in xml_blocks(xml, "Cluster") {
                if item.contains("<ClusterIdentifier>") {
                    clusters.push(Self::map_cluster(&item));
                }
            }
        }
        clusters
    }
}

#[async_trait]
impl CloudProvider for RedshiftAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Redshift
    }

    async fn test_connection(&self) -> UnifiedResult<ProviderInfo> {
        let started = Instant::now();
        self.describe_clusters(&[("MaxRecords", "1")]).await?;
        Ok(ProviderInfo {
            kind: "redshift".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: None,
            region: Some(self.params.region.clone()),
            detail: "Redshift DescribeClusters reachable".to_string(),
        })
    }

    async fn list_servers(&self) -> UnifiedResult<Vec<ServerInfo>> {
        let xml = self
            .describe_clusters(&[("MaxRecords", &MAX_LIST_LIMIT.to_string())])
            .await?;
        Ok(Self::parse_clusters(&xml))
    }

    async fn server_status(&self, id: &str) -> UnifiedResult<ServerInfo> {
        let xml = self
            .describe_clusters(&[("ClusterIdentifier", id)])
            .await?;
        Self::parse_clusters(&xml)
            .into_iter()
            .find(|cluster| cluster.id == id)
            .ok_or_else(|| UnifiedError::not_found(format!("Redshift cluster \"{id}\" not found")))
    }
}

/// Config-driven generic adapter: no provider-specific code, only the
/// config on the connection. See `CustomProviderConfig` for the
/// honest limits of template-based mapping.
pub struct CustomAdapter {
    pub(crate) client: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) auth: CustomAuth,
    pub(crate) config: CustomProviderConfig,
}

impl CustomAdapter {
    pub fn new(connection: &ProviderConnection) -> UnifiedResult<Self> {
        let config = connection.custom_config.clone().ok_or_else(|| {
            UnifiedError::bad_request("Custom connection requires custom_config")
        })?;
        validate_custom_config(&config)?;
        let endpoint = connection.endpoint.clone().unwrap_or_default();
        let url = Url::parse(endpoint.trim()).map_err(|_| {
            UnifiedError::bad_request("Invalid custom provider endpoint URL")
        })?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(UnifiedError::bad_request(
                "Custom provider endpoint must use http or https",
            ));
        }
        let auth = match config.auth_type.as_str() {
            "bearer" => {
                let token = connection
                    .credentials
                    .api_token
                    .clone()
                    .or_else(|| connection.credentials.bearer_token.clone())
                    .filter(|token| !token.trim().is_empty())
                    .ok_or_else(|| {
                        UnifiedError::bad_request(
                            "Custom provider with bearer auth requires api_token",
                        )
                    })?;
                CustomAuth::Bearer(token)
            }
            "basic" => {
                let username = connection
                    .credentials
                    .username
                    .clone()
                    .filter(|name| !name.trim().is_empty())
                    .ok_or_else(|| {
                        UnifiedError::bad_request(
                            "Custom provider with basic auth requires username",
                        )
                    })?;
                let password = connection
                    .credentials
                    .password
                    .clone()
                    .filter(|password| !password.trim().is_empty())
                    .ok_or_else(|| {
                        UnifiedError::bad_request(
                            "Custom provider with basic auth requires password",
                        )
                    })?;
                CustomAuth::Basic(username, password)
            }
            "api_key" => {
                let value = connection
                    .credentials
                    .api_token
                    .clone()
                    .filter(|token| !token.trim().is_empty())
                    .ok_or_else(|| {
                        UnifiedError::bad_request(
                            "Custom provider with api_key auth requires api_token",
                        )
                    })?;
                let header = config
                    .api_key_header
                    .clone()
                    .filter(|header| !header.trim().is_empty())
                    .unwrap_or_else(|| "X-API-Key".to_string());
                CustomAuth::ApiKey(header, value)
            }
            _ => CustomAuth::None,
        };
        Ok(Self {
            client: build_client()?,
            base_url: endpoint.trim().trim_end_matches('/').to_string(),
            auth,
            config,
        })
    }

    pub(crate) fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> UnifiedResult<reqwest::RequestBuilder> {
        let url = Url::parse(&format!("{}{path}", self.base_url))
            .map_err(|_| UnifiedError::internal("Invalid custom provider URL"))?;
        check_allowlist(&url, ProviderKind::Custom)?;
        let mut request = self.client.request(method, url);
        match &self.auth {
            CustomAuth::Bearer(token) => {
                request = request.headers(bearer_header(token)?);
            }
            CustomAuth::Basic(username, password) => {
                request = request.basic_auth(username, Some(password));
            }
            CustomAuth::ApiKey(header, value) => {
                let header_value = HeaderValue::from_str(value).map_err(|_| {
                    UnifiedError::bad_request(
                        "Stored credential contains characters invalid in an HTTP header",
                    )
                })?;
                let mut headers = HeaderMap::new();
                headers.insert(
                    reqwest::header::HeaderName::from_bytes(header.as_bytes()).map_err(|_| {
                        UnifiedError::bad_request(format!(
                            "Custom provider api_key_header \"{header}\" is not a valid header name"
                        ))
                    })?,
                    header_value,
                );
                request = request.headers(headers);
            }
            CustomAuth::None => {}
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        Ok(request)
    }

    pub(crate) fn optional_field(&self, value: &Value, path: &Option<String>) -> Option<String> {
        let path = path.as_deref()?;
        lookup_path(value, path).and_then(value_to_string)
    }

    pub(crate) fn map_item(&self, value: &Value) -> Option<ServerInfo> {
        let id = lookup_path(value, &self.config.fields.id).and_then(value_to_string)?;
        if id.trim().is_empty() {
            return None;
        }
        let name = lookup_path(value, &self.config.fields.name)
            .and_then(value_to_string)
            .unwrap_or_default();
        let raw_status = lookup_path(value, &self.config.fields.status)
            .and_then(value_to_string)
            .unwrap_or_default();
        Some(ServerInfo {
            id,
            name,
            item_kind: "server".to_string(),
            status: normalize_status(&raw_status).to_string(),
            raw_status: Some(raw_status),
            region: self.optional_field(value, &self.config.fields.region),
            public_ip: self.optional_field(value, &self.config.fields.public_ip),
            private_ip: self.optional_field(value, &self.config.fields.private_ip),
            cpu_cores: None,
            memory_mb: None,
            disk_gb: None,
            image: self.optional_field(value, &self.config.fields.image),
            created_at: self.optional_field(value, &self.config.fields.created_at),
        })
    }

    pub(crate) fn unwrap_item<'a>(&self, body: &'a Value) -> Option<&'a Value> {
        match &self.config.item_response_key {
            Some(key) => body.get(key),
            None => Some(body),
        }
    }

    pub(crate) fn render_template_body(
        &self,
        template: &Option<String>,
        default_template: &str,
        vars: &[(&str, &str)],
    ) -> UnifiedResult<Value> {
        let template = template.as_deref().unwrap_or(default_template);
        let rendered = render_body_template(template, vars);
        serde_json::from_str::<Value>(&rendered).map_err(|_| {
            UnifiedError::bad_request(
                "Custom provider body template did not render to valid JSON",
            )
        })
    }
}

#[async_trait]
impl CloudProvider for CustomAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Custom
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.config.capabilities()
    }

    async fn test_connection(&self) -> UnifiedResult<ProviderInfo> {
        let started = Instant::now();
        let request = self.request(Method::GET, &self.config.list_path, None)?;
        send_json(&self.client, request, "custom").await?;
        Ok(ProviderInfo {
            kind: "custom".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: None,
            region: None,
            detail: "Custom provider API reachable".to_string(),
        })
    }

    async fn list_servers(&self) -> UnifiedResult<Vec<ServerInfo>> {
        let request = self.request(Method::GET, &self.config.list_path, None)?;
        let body = send_json(&self.client, request, "custom").await?;
        let items = match &self.config.list_response_key {
            Some(key) => body
                .get(key)
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    UnifiedError::internal(format!(
                        "Custom provider list response has no array at key \"{key}\""
                    ))
                })?
                .clone(),
            None => body.as_array().cloned().ok_or_else(|| {
                UnifiedError::internal(
                    "Custom provider list response is not a JSON array and no list_response_key is configured",
                )
            })?,
        };
        Ok(items.iter().filter_map(|item| self.map_item(item)).collect())
    }

    async fn server_status(&self, id: &str) -> UnifiedResult<ServerInfo> {
        let path = self
            .config
            .status_path
            .replace("{id}", &encode_path_segment(id));
        let request = self.request(Method::GET, &path, None)?;
        let body = send_json(&self.client, request, "custom").await?;
        self.unwrap_item(&body)
            .and_then(|item| self.map_item(item))
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Custom provider server \"{id}\" not found"))
            })
    }

    async fn create_server(&self, request: &CreateServerRequest) -> UnifiedResult<ServerInfo> {
        let Some(create_path) = &self.config.create_path else {
            return Err(unsupported(self.kind(), "create", "supports_create"));
        };
        let body = self.render_template_body(
            &self.config.create_body_template,
            "{\"name\":\"{name}\",\"size\":\"{server_type}\",\"image\":\"{image}\",\"region\":\"{region}\"}",
            &[
                ("name", &request.name),
                ("server_type", request.server_type.as_deref().unwrap_or("")),
                (
                    "image",
                    request
                        .image_id
                        .as_deref()
                        .or(request.image.as_deref())
                        .unwrap_or(""),
                ),
                ("region", request.region.as_deref().unwrap_or("")),
            ],
        )?;
        let http_request = self.request(Method::POST, create_path, Some(body))?;
        let response = send_json(&self.client, http_request, "custom").await?;
        self.unwrap_item(&response)
            .and_then(|item| self.map_item(item))
            .ok_or_else(|| {
                UnifiedError::internal(
                    "Custom provider create response has no mappable server object",
                )
            })
    }

    async fn delete_server(&self, id: &str) -> UnifiedResult<()> {
        let Some(delete_path) = &self.config.delete_path else {
            return Err(unsupported(self.kind(), "delete", "supports_delete"));
        };
        let path = delete_path.replace("{id}", &encode_path_segment(id));
        let request = self.request(Method::DELETE, &path, None)?;
        send_no_content(&self.client, request, "custom").await
    }

    async fn start_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        self.power(id, true).await
    }

    async fn stop_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        self.power(id, false).await
    }
}

impl CustomAdapter {
    pub(crate) async fn power(&self, id: &str, start: bool) -> UnifiedResult<ServerInfo> {
        let (Some(power_path), Some(actions)) =
            (&self.config.power_path, &self.config.power_actions)
        else {
            return Err(unsupported(self.kind(), "power", "supports_start_stop"));
        };
        let action = if start { &actions.start } else { &actions.stop };
        let path = power_path
            .replace("{id}", &encode_path_segment(id))
            .replace("{action}", &encode_path_segment(action));
        let body = self.render_template_body(
            &self.config.power_body_template,
            "{\"type\":\"{action}\"}",
            &[("action", action)],
        )?;
        let request = self.request(Method::POST, &path, Some(body))?;
        send_no_content(&self.client, request, "custom").await?;
        self.server_status(id).await
    }
}
