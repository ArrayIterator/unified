// providers module -- Provider adapters: Hetzner, OVH, Tencent, and Huawei.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use reqwest::Method;
use serde_json::Value;
use std::sync::RwLock;
use std::time::{Duration, Instant};
use url::Url;

use super::*;

pub struct HetznerAdapter {
    pub(crate) client: reqwest::Client,
    pub(crate) token: String,
}

impl HetznerAdapter {
    pub fn new(connection: &ProviderConnection) -> UnifiedResult<Self> {
        let token = connection
            .credentials
            .api_token
            .clone()
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| UnifiedError::bad_request("Hetzner connection requires api_token"))?;
        Ok(Self {
            client: build_client()?,
            token,
        })
    }

    pub(crate) async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> UnifiedResult<Value> {
        let url = Url::parse(&format!("https://api.hetzner.cloud/v1{path}"))
            .map_err(|_| UnifiedError::internal("Invalid Hetzner API URL"))?;
        check_allowlist(&url, ProviderKind::Hetzner)?;
        let mut request = self.client.request(method, url);
        let headers = bearer_header(&self.token)?;
        request = request.headers(headers);
        if let Some(body) = body {
            request = request.json(&body);
        }
        send_json(&self.client, request, "hetzner").await
    }

    pub(crate) fn map_server(value: &Value) -> ServerInfo {
        let id = value
            .get("id")
            .map(|id| id.to_string())
            .unwrap_or_default();
        let raw_status = json_string(value, "status").unwrap_or("").to_string();
        let region = value
            .get("datacenter")
            .and_then(|dc| dc.get("location"))
            .and_then(|loc| json_string(loc, "name"))
            .map(str::to_string);
        let server_type = value.get("server_type");
        let public_ip = value
            .get("public_net")
            .and_then(|net| net.get("ipv4"))
            .and_then(|ipv4| json_string(ipv4, "ip"))
            .map(str::to_string);
        let private_ip = value
            .get("private_net")
            .and_then(|nets| nets.as_array())
            .and_then(|nets| nets.first())
            .and_then(|net| json_string(net, "ip"))
            .map(str::to_string);
        ServerInfo {
            id,
            name: json_string(value, "name").unwrap_or("").to_string(),
            item_kind: "server".to_string(),
            status: normalize_status(&raw_status).to_string(),
            raw_status: Some(raw_status),
            region,
            public_ip,
            private_ip,
            cpu_cores: server_type.and_then(|st| json_u64(st, "cores")).map(|cores| cores as u32),
            memory_mb: server_type
                .and_then(|st| st.get("memory"))
                .and_then(Value::as_f64)
                .map(|gb| (gb * 1024.0) as u64),
            disk_gb: server_type.and_then(|st| json_u64(st, "disk")),
            image: value
                .get("image")
                .and_then(|image| json_string(image, "name"))
                .map(str::to_string),
            created_at: json_string(value, "created").map(str::to_string),
        }
    }
}

#[async_trait]
impl CloudProvider for HetznerAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Hetzner
    }

    fn metadata(&self) -> ProviderMetadata {
        metadata_with_version(self.kind(), "v1")
    }

    async fn test_connection(&self) -> UnifiedResult<ProviderInfo> {
        let started = Instant::now();
        self.request(Method::GET, "/servers?per_page=1", None).await?;
        Ok(ProviderInfo {
            kind: "hetzner".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: None,
            region: None,
            detail: "Hetzner Cloud API reachable".to_string(),
        })
    }

    async fn list_servers(&self) -> UnifiedResult<Vec<ServerInfo>> {
        let body = self
            .request(Method::GET, &format!("/servers?per_page={MAX_LIST_LIMIT}"), None)
            .await?;
        let servers = body
            .get("servers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(servers.iter().map(Self::map_server).collect())
    }

    async fn server_status(&self, id: &str) -> UnifiedResult<ServerInfo> {
        if id.trim().is_empty() || id.parse::<u64>().is_err() {
            return Err(UnifiedError::bad_request("Hetzner server id must be numeric"));
        }
        let body = self.request(Method::GET, &format!("/servers/{id}"), None).await?;
        body.get("server")
            .map(Self::map_server)
            .ok_or_else(|| UnifiedError::not_found(format!("Hetzner server \"{id}\" not found")))
    }

    async fn create_server(&self, request: &CreateServerRequest) -> UnifiedResult<ServerInfo> {
        if request.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Server name is required"));
        }
        let server_type = request.server_type.clone().ok_or_else(|| {
            UnifiedError::bad_request("Hetzner create requires server_type (for example \"cx22\")")
        })?;
        let image = request
            .image_id
            .clone()
            .or_else(|| request.image.clone())
            .ok_or_else(|| {
                UnifiedError::bad_request("Hetzner create requires image or image_id")
            })?;
        let mut payload = serde_json::json!({
            "name": request.name,
            "server_type": server_type,
            "image": image,
            "start_after_create": true,
        });
        if let Some(region) = &request.region {
            payload["location"] = Value::String(region.clone());
        }
        if let Some(keys) = &request.ssh_key_ids {
            payload["ssh_keys"] = Value::Array(
                keys.iter().map(|key| Value::String(key.clone())).collect(),
            );
        }
        let body = self.request(Method::POST, "/servers", Some(payload)).await?;
        body.get("server")
            .map(Self::map_server)
            .ok_or_else(|| {
                UnifiedError::internal("Hetzner create succeeded but returned no server")
            })
    }

    async fn scale_server(
        &self,
        id: &str,
        request: &ScaleServerRequest,
    ) -> UnifiedResult<ServerInfo> {
        let server_type = request.server_type.clone().ok_or_else(|| {
            UnifiedError::bad_request("Hetzner scale requires server_type (the new server type)")
        })?;
        let payload = serde_json::json!({
            "server_type": server_type,
            "upgrade_disk": false,
        });
        self.request(
            Method::POST,
            &format!("/servers/{id}/actions/change_type"),
            Some(payload),
        )
        .await?;
        self.server_status(id).await
    }

    async fn delete_server(&self, id: &str) -> UnifiedResult<()> {
        self.request(Method::DELETE, &format!("/servers/{id}"), None)
            .await?;
        Ok(())
    }

    async fn usage_metrics(&self, id: &str) -> UnifiedResult<UsageMetrics> {
        // Last hour of metrics. The `cpu` series is a percentage; the
        // `network.0.in` / `network.0.out` series are bytes per second,
        // integrated over the step to total bytes. Memory and disk
        // usage are not exposed by this API.
        let end = Utc::now();
        let start = DateTime::from_timestamp(end.timestamp() - 3600, 0).unwrap_or(end);
        let fmt = |time: DateTime<Utc>| time.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let body = self
            .request(
                Method::GET,
                &format!(
                    "/servers/{}/metrics?type=cpu,network&start={}&end={}",
                    encode_path_segment(id),
                    fmt(start),
                    fmt(end),
                ),
                None,
            )
            .await?;
        let metrics = body.get("metrics");
        let series = metrics.and_then(|metrics| metrics.get("time_series"));
        let last_value = |key: &str| -> Option<f64> {
            series
                .and_then(|series| series.get(key))
                .and_then(|series| series.get("values"))
                .and_then(Value::as_array)
                .and_then(|values| values.last())
                .and_then(|point| point.as_array())
                .and_then(|point| point.get(1))
                .and_then(|value| value.as_str())
                .and_then(|value| value.parse::<f64>().ok())
        };
        let integrate = |key: &str| -> f64 {
            let step = metrics
                .and_then(|metrics| metrics.get("step"))
                .and_then(Value::as_f64)
                .unwrap_or(60.0);
            series
                .and_then(|series| series.get(key))
                .and_then(|series| series.get("values"))
                .and_then(Value::as_array)
                .map(|values| {
                    values.iter().fold(0.0, |total, point| {
                        let value = point
                            .as_array()
                            .and_then(|point| point.get(1))
                            .and_then(|value| value.as_str())
                            .and_then(|value| value.parse::<f64>().ok())
                            .unwrap_or(0.0);
                        total + value * step
                    })
                })
                .unwrap_or(0.0)
        };
        Ok(UsageMetrics {
            server_id: id.to_string(),
            cpu_percent: last_value("cpu"),
            memory_mb: None,
            disk_gb: None,
            bandwidth_in_mb: Some(integrate("network.0.in") / (1024.0 * 1024.0)),
            bandwidth_out_mb: Some(integrate("network.0.out") / (1024.0 * 1024.0)),
            sampled_at: end.to_rfc3339(),
        })
    }
}

// ---------------------------------------------------------------------------
// OVH adapter.
// ---------------------------------------------------------------------------

/// OVH API (`api.ovh.com` / `eu.api.ovh.com`), application-key +
/// signature auth. One cloud project per connection.
pub struct OvhAdapter {
    pub(crate) client: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) application_key: String,
    pub(crate) application_secret: String,
    pub(crate) consumer_key: String,
    pub(crate) project_id: String,
}

impl OvhAdapter {
    pub fn new(connection: &ProviderConnection) -> UnifiedResult<Self> {
        let creds = &connection.credentials;
        let base_url = connection
            .endpoint
            .clone()
            .unwrap_or_else(|| "https://eu.api.ovh.com/1.0".to_string());
        let url = Url::parse(&base_url)
            .map_err(|_| UnifiedError::bad_request("Invalid OVH endpoint URL"))?;
        if !ProviderKind::Ovh.allowed_hosts().contains(&url.host_str().unwrap_or("")) {
            return Err(UnifiedError::bad_request(
                "OVH endpoint must be https://api.ovh.com/1.0 or https://eu.api.ovh.com/1.0",
            ));
        }
        Ok(Self {
            client: build_client()?,
            base_url: base_url.trim_end_matches('/').to_string(),
            application_key: creds.application_key.clone().unwrap_or_default(),
            application_secret: creds.application_secret.clone().unwrap_or_default(),
            consumer_key: creds.consumer_key.clone().unwrap_or_default(),
            project_id: connection.project_id.clone().unwrap_or_default(),
        })
    }

    pub(crate) async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> UnifiedResult<Value> {
        let url_string = format!("{}{path}", self.base_url);
        let url = Url::parse(&url_string)
            .map_err(|_| UnifiedError::internal("Invalid OVH API URL"))?;
        check_allowlist(&url, ProviderKind::Ovh)?;
        let body_string = body
            .as_ref()
            .map(|body| body.to_string())
            .unwrap_or_default();
        let timestamp = Utc::now().timestamp();
        let signature = ovh_signature(
            &self.application_secret,
            &self.consumer_key,
            method.as_str(),
            &url_string,
            &body_string,
            timestamp,
        );
        let mut headers = HeaderMap::new();
        let set = |headers: &mut HeaderMap, name: &str, value: &str| -> UnifiedResult<()> {
            let header_value = HeaderValue::from_str(value).map_err(|_| {
                UnifiedError::bad_request("OVH credential contains characters invalid in an HTTP header")
            })?;
            headers.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                    UnifiedError::internal("Invalid header name while signing an OVH request")
                })?,
                header_value,
            );
            Ok(())
        };
        set(&mut headers, "x-ovh-application", &self.application_key)?;
        set(&mut headers, "x-ovh-consumer", &self.consumer_key)?;
        set(&mut headers, "x-ovh-timestamp", &timestamp.to_string())?;
        set(&mut headers, "x-ovh-signature", &signature)?;
        if body.is_some() {
            set(&mut headers, "content-type", "application/json")?;
        }
        let mut request = self.client.request(method, url).headers(headers);
        if let Some(body) = body {
            request = request.json(&body);
        }
        send_json(&self.client, request, "ovh").await
    }

    pub(crate) fn map_instance(value: &Value) -> ServerInfo {
        let raw_status = json_string(value, "status").unwrap_or("").to_string();
        let mut public_ip = None;
        let mut private_ip = None;
        if let Some(addresses) = value.get("ipAddresses").and_then(Value::as_array) {
            for address in addresses {
                match json_string(address, "type") {
                    Some("public") => {
                        public_ip = json_string(address, "ip").map(str::to_string);
                    }
                    Some("private") => {
                        private_ip = json_string(address, "ip").map(str::to_string);
                    }
                    _ => {}
                }
            }
        }
        let flavor = value.get("flavor");
        ServerInfo {
            id: json_string(value, "id").unwrap_or("").to_string(),
            name: json_string(value, "name").unwrap_or("").to_string(),
            item_kind: "server".to_string(),
            status: normalize_status(&raw_status).to_string(),
            raw_status: Some(raw_status),
            region: json_string(value, "region").map(str::to_string),
            public_ip,
            private_ip,
            cpu_cores: flavor.and_then(|flavor| json_u64(flavor, "vcpus")).map(|cores| cores as u32),
            memory_mb: flavor.and_then(|flavor| json_u64(flavor, "ram")),
            disk_gb: flavor.and_then(|flavor| json_u64(flavor, "disk")),
            image: value
                .get("image")
                .and_then(|image| json_string(image, "name"))
                .map(str::to_string),
            created_at: json_string(value, "created").map(str::to_string),
        }
    }
}

#[async_trait]
impl CloudProvider for OvhAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Ovh
    }

    async fn test_connection(&self) -> UnifiedResult<ProviderInfo> {
        let started = Instant::now();
        self.request(
            Method::GET,
            &format!("/cloud/project/{}/instance", self.project_id),
            None,
        )
        .await?;
        Ok(ProviderInfo {
            kind: "ovh".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: Some(self.project_id.clone()),
            region: None,
            detail: "OVHcloud API reachable for the configured project".to_string(),
        })
    }

    async fn list_servers(&self) -> UnifiedResult<Vec<ServerInfo>> {
        let body = self
            .request(
                Method::GET,
                &format!("/cloud/project/{}/instance", self.project_id),
                None,
            )
            .await?;
        let instances = body.as_array().cloned().unwrap_or_default();
        Ok(instances.iter().map(Self::map_instance).collect())
    }

    async fn server_status(&self, id: &str) -> UnifiedResult<ServerInfo> {
        let body = self
            .request(
                Method::GET,
                &format!("/cloud/project/{}/instance/{id}", self.project_id),
                None,
            )
            .await?;
        Ok(Self::map_instance(&body))
    }

    async fn create_server(&self, request: &CreateServerRequest) -> UnifiedResult<ServerInfo> {
        if request.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Server name is required"));
        }
        let flavor_id = request.server_type.clone().ok_or_else(|| {
            UnifiedError::bad_request("OVH create requires server_type (the flavor id)")
        })?;
        let image_id = request.image_id.clone().or_else(|| request.image.clone()).ok_or_else(|| {
            UnifiedError::bad_request("OVH create requires image or image_id (the image id)")
        })?;
        let region = request.region.clone().ok_or_else(|| {
            UnifiedError::bad_request("OVH create requires region (for example \"GRA11\")")
        })?;
        let mut payload = serde_json::json!({
            "flavorId": flavor_id,
            "imageId": image_id,
            "region": region,
            "name": request.name,
        });
        if let Some(keys) = &request.ssh_key_ids {
            if let Some(first) = keys.first() {
                payload["sshKeyId"] = Value::String(first.clone());
            }
        }
        let body = self
            .request(
                Method::POST,
                &format!("/cloud/project/{}/instance", self.project_id),
                Some(payload),
            )
            .await?;
        Ok(Self::map_instance(&body))
    }

    async fn scale_server(
        &self,
        id: &str,
        request: &ScaleServerRequest,
    ) -> UnifiedResult<ServerInfo> {
        let flavor_id = request.server_type.clone().ok_or_else(|| {
            UnifiedError::bad_request("OVH scale requires server_type (the new flavor id)")
        })?;
        let payload = serde_json::json!({ "flavorId": flavor_id });
        self.request(
            Method::POST,
            &format!("/cloud/project/{}/instance/{id}/resize", self.project_id),
            Some(payload),
        )
        .await?;
        self.server_status(id).await
    }

    async fn delete_server(&self, id: &str) -> UnifiedResult<()> {
        self.request(
            Method::DELETE,
            &format!("/cloud/project/{}/instance/{id}", self.project_id),
            None,
        )
        .await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tencent Cloud CVM adapter.
// ---------------------------------------------------------------------------

/// Tencent Cloud CVM (`cvm.tencentcloudapi.com`), TC3-HMAC-SHA256
/// signing. List / status only in this round.
pub struct TencentAdapter {
    pub(crate) client: reqwest::Client,
    pub(crate) secret_id: String,
    pub(crate) secret_key: String,
    pub(crate) region: String,
}

impl TencentAdapter {
    pub fn new(connection: &ProviderConnection) -> UnifiedResult<Self> {
        Ok(Self {
            client: build_client()?,
            secret_id: connection
                .credentials
                .access_key_id
                .clone()
                .unwrap_or_default(),
            secret_key: connection
                .credentials
                .secret_access_key
                .clone()
                .unwrap_or_default(),
            region: connection.region.clone().unwrap_or_default(),
        })
    }

    pub(crate) async fn call(&self, action: &str, params: Value) -> UnifiedResult<Value> {
        let host = "cvm.tencentcloudapi.com";
        let url = Url::parse(&format!("https://{host}/"))
            .map_err(|_| UnifiedError::internal("Invalid Tencent API URL"))?;
        check_allowlist(&url, ProviderKind::Tencent)?;
        let payload = params.to_string();
        let tc3 = Tc3Params {
            secret_id: self.secret_id.clone(),
            secret_key: self.secret_key.clone(),
            region: self.region.clone(),
            service: "cvm".to_string(),
            action: action.to_string(),
            version: "2017-03-12".to_string(),
        };
        let headers = tc3_headers(&tc3, host, payload.as_bytes(), Utc::now())?;
        let request = self
            .client
            .post(url)
            .headers(headers)
            .body(payload);
        let body = send_json(&self.client, request, "tencent").await?;
        // Tencent wraps errors as {"Response": {"Error": {...}}}.
        if let Some(error) = body.get("Response").and_then(|response| response.get("Error")) {
            let code = json_string(error, "Code").unwrap_or("TencentError");
            let message = json_string(error, "Message").unwrap_or("Tencent Cloud request failed");
            return Err(UnifiedError::internal(format!("Tencent Cloud error {code}: {message}")));
        }
        Ok(body)
    }

    pub(crate) fn map_instance(value: &Value) -> ServerInfo {
        let raw_status = json_string(value, "InstanceState").unwrap_or("").to_string();
        let public_ips = value
            .get("PublicIpAddresses")
            .and_then(Value::as_array)
            .and_then(|ips| ips.first())
            .and_then(Value::as_str)
            .map(str::to_string);
        let private_ips = value
            .get("PrivateIpAddresses")
            .and_then(Value::as_array)
            .and_then(|ips| ips.first())
            .and_then(Value::as_str)
            .map(str::to_string);
        let memory_gb = json_u64(value, "Memory").unwrap_or(0);
        ServerInfo {
            id: json_string(value, "InstanceId").unwrap_or("").to_string(),
            name: json_string(value, "InstanceName").unwrap_or("").to_string(),
            item_kind: "server".to_string(),
            status: normalize_status(&raw_status).to_string(),
            raw_status: Some(raw_status),
            region: value
                .get("Placement")
                .and_then(|placement| json_string(placement, "Zone"))
                .map(str::to_string),
            public_ip: public_ips,
            private_ip: private_ips,
            cpu_cores: json_u64(value, "CPU").map(|cores| cores as u32),
            memory_mb: if memory_gb > 0 { Some(memory_gb * 1024) } else { None },
            disk_gb: None,
            image: json_string(value, "ImageId").map(str::to_string),
            created_at: json_string(value, "CreatedTime").map(str::to_string),
        }
    }
}

#[async_trait]
impl CloudProvider for TencentAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Tencent
    }

    async fn test_connection(&self) -> UnifiedResult<ProviderInfo> {
        let started = Instant::now();
        self.call(
            "DescribeInstances",
            serde_json::json!({ "Limit": 1, "Offset": 0 }),
        )
        .await?;
        Ok(ProviderInfo {
            kind: "tencent".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: None,
            region: Some(self.region.clone()),
            detail: "Tencent Cloud CVM API reachable".to_string(),
        })
    }

    async fn list_servers(&self) -> UnifiedResult<Vec<ServerInfo>> {
        let body = self
            .call(
                "DescribeInstances",
                serde_json::json!({ "Limit": MAX_LIST_LIMIT, "Offset": 0 }),
            )
            .await?;
        let instances = body
            .get("Response")
            .and_then(|response| response.get("InstanceSet"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(instances.iter().map(Self::map_instance).collect())
    }

    async fn server_status(&self, id: &str) -> UnifiedResult<ServerInfo> {
        let body = self
            .call(
                "DescribeInstances",
                serde_json::json!({
                    "Filters": [{ "Name": "instance-id", "Values": [id] }],
                    "Limit": 1,
                    "Offset": 0,
                }),
            )
            .await?;
        let instances = body
            .get("Response")
            .and_then(|response| response.get("InstanceSet"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        instances
            .first()
            .map(Self::map_instance)
            .ok_or_else(|| UnifiedError::not_found(format!("Tencent instance \"{id}\" not found")))
    }
}

// ---------------------------------------------------------------------------
// Huawei Cloud ECS adapter.
// ---------------------------------------------------------------------------

/// Huawei Cloud ECS. Two auth modes: IAM password credentials (a token
/// is fetched from the regional IAM endpoint and cached until shortly
/// before expiry) or AK/SK request signing (SDK-HMAC-SHA256) against
/// the ECS endpoint. List / status only in this round.
pub struct HuaweiAdapter {
    pub(crate) client: reqwest::Client,
    pub(crate) region: String,
    pub(crate) username: Option<String>,
    pub(crate) password: Option<String>,
    pub(crate) access_key: Option<String>,
    pub(crate) secret_key: Option<String>,
    pub(crate) project_id: Option<String>,
    /// Cached IAM token: (token, project id, expiry).
    pub(crate) token_cache: RwLock<Option<(String, String, DateTime<Utc>)>>,
}

impl HuaweiAdapter {
    pub fn new(connection: &ProviderConnection) -> UnifiedResult<Self> {
        let creds = &connection.credentials;
        let by_keys = creds
            .access_key_id
            .as_deref()
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
            && creds
                .secret_access_key
                .as_deref()
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false);
        if by_keys && connection.project_id.as_deref().map(str::is_empty).unwrap_or(true) {
            return Err(UnifiedError::bad_request(
                "Huawei AK/SK mode requires project_id (the ECS project id)",
            ));
        }
        Ok(Self {
            client: build_client()?,
            region: connection.region.clone().unwrap_or_default(),
            username: creds.username.clone(),
            password: creds.password.clone(),
            access_key: creds.access_key_id.clone(),
            secret_key: creds.secret_access_key.clone(),
            project_id: connection.project_id.clone(),
            token_cache: RwLock::new(None),
        })
    }

    pub(crate) fn ecs_base(&self) -> UnifiedResult<String> {
        self.huawei_base("ecs")
    }

    pub(crate) fn iam_base(&self) -> UnifiedResult<String> {
        self.huawei_base("iam")
    }

    /// Build a Huawei API base URL for one service, refusing any
    /// region that could change the host. The region is validated
    /// against `^[a-z0-9-]+$` first -- a region such as
    /// `x.myhuaweicloud.com@evil.example` would otherwise smuggle a
    /// user-info part into the URL and send the IAM password (or an
    /// AK/SK-signed request) to another host -- and the built host
    /// must end with `.myhuaweicloud.com`, mirroring the AWS
    /// endpoint check.
    fn huawei_base(&self, service: &str) -> UnifiedResult<String> {
        if !is_valid_huawei_region(&self.region) {
            return Err(UnifiedError::bad_request(format!(
                "Invalid Huawei region \"{}\" (expected ^[a-z0-9-]+$)",
                self.region
            )));
        }
        let base = format!("https://{service}.{}.myhuaweicloud.com", self.region);
        let url = Url::parse(&base)
            .map_err(|_| UnifiedError::internal("Invalid Huawei API URL"))?;
        let host = url.host_str().unwrap_or("");
        if !host.ends_with(".myhuaweicloud.com") {
            return Err(UnifiedError::bad_request(
                "Invalid Huawei API endpoint host",
            ));
        }
        Ok(base)
    }

    /// Fetch (or reuse) an IAM token for the password mode.
    pub(crate) async fn iam_token(&self) -> UnifiedResult<(String, String)> {
        {
            let cache = self.token_cache.read().expect("token cache lock poisoned");
            if let Some((token, project_id, expires_at)) = cache.as_ref() {
                if *expires_at > Utc::now() + chrono::Duration::seconds(60) {
                    return Ok((token.clone(), project_id.clone()));
                }
            }
        }
        let username = self.username.clone().unwrap_or_default();
        let password = self.password.clone().unwrap_or_default();
        let project_name = self.project_id.clone().unwrap_or_default();
        let mut payload = serde_json::json!({
            "auth": {
                "identity": {
                    "methods": ["password"],
                    "password": {
                        "user": {
                            "name": username,
                            "password": password,
                            "domain": { "name": username },
                        },
                    },
                },
                "scope": { "project": { "name": project_name } },
            },
        });
        if project_name.is_empty() {
            payload["auth"]["scope"] = serde_json::json!({});
        }
        let url = Url::parse(&format!("{}/v3/auth/tokens", self.iam_base()?))
            .map_err(|_| UnifiedError::internal("Invalid Huawei IAM URL"))?;
        let response = self
            .client
            .post(url)
            .json(&payload)
            .send()
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("Huawei IAM request failed: {error}"))
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(http_status_error(status, "huawei", parse_retry_after(&response)));
        }
        let token = response
            .headers()
            .get("x-subject-token")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
            .ok_or_else(|| {
                UnifiedError::internal("Huawei IAM response carried no token")
            })?;
        let body: Value = response.json().await.map_err(|error| {
            UnifiedError::internal(format!("Huawei IAM response unreadable: {error}"))
        })?;
        let project_id = body
            .get("token")
            .and_then(|token| token.get("project"))
            .and_then(|project| json_string(project, "id"))
            .map(str::to_string)
            .or_else(|| self.project_id.clone())
            .ok_or_else(|| {
                UnifiedError::internal("Huawei IAM response carried no project id")
            })?;
        let expires_at = body
            .get("token")
            .and_then(|token| json_string(token, "expires_at"))
            .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
            .map(|time| time.with_timezone(&Utc))
            .unwrap_or_else(|| Utc::now() + chrono::Duration::hours(1));
        {
            let mut cache = self.token_cache.write().expect("token cache lock poisoned");
            *cache = Some((token.clone(), project_id.clone(), expires_at));
        }
        Ok((token, project_id))
    }

    /// Build the auth headers for one ECS request: IAM token when
    /// password credentials are configured, AK/SK signing otherwise.
    pub(crate) async fn auth_headers(
        &self,
        method: &str,
        url: &Url,
        payload: &[u8],
    ) -> UnifiedResult<HeaderMap> {
        let mut headers = HeaderMap::new();
        if self.username.is_some() && self.password.is_some() {
            let (token, _) = self.iam_token().await?;
            let value = HeaderValue::from_str(&format!("X-Auth-Token {token}")).map_err(|_| {
                UnifiedError::bad_request("Huawei token contains characters invalid in an HTTP header")
            })?;
            headers.insert("x-auth-token", value);
        } else {
            let (authorization, sdk_date) = huawei_authorization(
                self.access_key.as_deref().unwrap_or(""),
                self.secret_key.as_deref().unwrap_or(""),
                &self.region,
                "ecs",
                method,
                url,
                payload,
                Utc::now(),
            )?;
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&authorization).map_err(|_| {
                    UnifiedError::internal("Computed Huawei signature is not a valid header value")
                })?,
            );
            headers.insert(
                "x-sdk-date",
                HeaderValue::from_str(&sdk_date).map_err(|_| {
                    UnifiedError::internal("Computed Huawei date is not a valid header value")
                })?,
            );
        }
        Ok(headers)
    }

    pub(crate) async fn request(&self, method: Method, path: &str) -> UnifiedResult<Value> {
        // The project id comes from the IAM token in password mode, or
        // from the connection in AK/SK mode.
        let project_id = if self.username.is_some() && self.password.is_some() {
            self.iam_token().await?.1
        } else {
            self.project_id.clone().unwrap_or_default()
        };
        let url = Url::parse(&format!("{}/v2/{project_id}{path}", self.ecs_base()?))
            .map_err(|_| UnifiedError::internal("Invalid Huawei ECS URL"))?;
        let headers = self.auth_headers(method.as_str(), &url, b"").await?;
        let request = self.client.request(method, url).headers(headers);
        send_json(&self.client, request, "huawei").await
    }

    pub(crate) fn map_server(value: &Value) -> ServerInfo {
        let raw_status = json_string(value, "status").unwrap_or("").to_string();
        let mut public_ip = None;
        let mut private_ip = None;
        if let Some(addresses) = value.get("addresses").and_then(Value::as_object) {
            for (_, entries) in addresses {
                if let Some(entries) = entries.as_array() {
                    for entry in entries {
                        match json_string(entry, "OS-EXT-IPS:type") {
                            Some("floating") => {
                                public_ip = json_string(entry, "addr").map(str::to_string);
                            }
                            Some("fixed") => {
                                private_ip = json_string(entry, "addr").map(str::to_string);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        let flavor = value.get("flavor");
        ServerInfo {
            id: json_string(value, "id").unwrap_or("").to_string(),
            name: json_string(value, "name").unwrap_or("").to_string(),
            item_kind: "server".to_string(),
            status: normalize_status(&raw_status).to_string(),
            raw_status: Some(raw_status),
            region: None,
            public_ip,
            private_ip,
            cpu_cores: flavor.and_then(|flavor| json_u64(flavor, "vcpus")).map(|cores| cores as u32),
            memory_mb: flavor.and_then(|flavor| json_u64(flavor, "ram")),
            disk_gb: flavor.and_then(|flavor| json_u64(flavor, "disk")),
            image: value
                .get("image")
                .and_then(|image| json_string(image, "id"))
                .map(str::to_string),
            created_at: json_string(value, "created").map(str::to_string),
        }
    }
}

#[async_trait]
impl CloudProvider for HuaweiAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Huawei
    }

    async fn test_connection(&self) -> UnifiedResult<ProviderInfo> {
        let started = Instant::now();
        // In password mode this validates the IAM token flow; in AK/SK
        // mode it validates the signed ECS call.
        self.request(Method::GET, "/servers/detail?limit=1").await?;
        Ok(ProviderInfo {
            kind: "huawei".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: self.project_id.clone(),
            region: Some(self.region.clone()),
            detail: "Huawei Cloud ECS API reachable".to_string(),
        })
    }

    async fn list_servers(&self) -> UnifiedResult<Vec<ServerInfo>> {
        let body = self
            .request(Method::GET, &format!("/servers/detail?limit={MAX_LIST_LIMIT}"))
            .await?;
        let servers = body
            .get("servers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(servers.iter().map(Self::map_server).collect())
    }

    async fn server_status(&self, id: &str) -> UnifiedResult<ServerInfo> {
        let body = self.request(Method::GET, &format!("/servers/{id}")).await?;
        body.get("server")
            .map(Self::map_server)
            .ok_or_else(|| UnifiedError::not_found(format!("Huawei server \"{id}\" not found")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn huawei_connection(region: &str) -> ProviderConnection {
        ProviderConnection {
            id: Uuid::now_v7(),
            name: "huawei".to_string(),
            kind: ProviderKind::Huawei,
            endpoint: None,
            region: Some(region.to_string()),
            project_id: None,
            credentials: ProviderCredentials {
                username: Some("user".to_string()),
                password: Some("secret".to_string()),
                ..Default::default()
            },
            custom_config: None,
            enabled: true,
            last_tested_at: None,
            last_status: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn huawei_ecs_base_accepts_valid_region() {
        let adapter = HuaweiAdapter::new(&huawei_connection("cn-north-4")).unwrap();
        assert_eq!(
            adapter.ecs_base().unwrap(),
            "https://ecs.cn-north-4.myhuaweicloud.com"
        );
    }

    #[test]
    fn huawei_iam_base_accepts_valid_region() {
        let adapter = HuaweiAdapter::new(&huawei_connection("cn-north-4")).unwrap();
        assert_eq!(
            adapter.iam_base().unwrap(),
            "https://iam.cn-north-4.myhuaweicloud.com"
        );
    }

    #[test]
    fn huawei_bases_reject_host_injection_region() {
        let adapter =
            HuaweiAdapter::new(&huawei_connection("x.myhuaweicloud.com@evil.example")).unwrap();
        assert!(adapter.ecs_base().is_err());
        assert!(adapter.iam_base().is_err());
    }

    #[test]
    fn huawei_bases_reject_uppercase_and_slash_regions() {
        for region in ["CN-North-4", "cn-north-4/", "cn north-4"] {
            let adapter = HuaweiAdapter::new(&huawei_connection(region)).unwrap();
            assert!(adapter.ecs_base().is_err(), "region {region} must be rejected");
            assert!(adapter.iam_base().is_err(), "region {region} must be rejected");
        }
    }
}
