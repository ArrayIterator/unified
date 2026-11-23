// providers module -- Provider adapters: AWS, Google, Virtualizor, Docker, and Kubernetes.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Method;
use serde_json::Value;
use std::time::Instant;
use url::Url;

use super::*;

// ---------------------------------------------------------------------------
// AWS EC2 adapter.
// ---------------------------------------------------------------------------

/// AWS EC2 (`ec2.<region>.amazonaws.com`), SigV4 signed query API.
/// DescribeInstances only in this round; the XML response is read with
/// the minimal extractor above.
pub struct AwsAdapter {
    pub(crate) client: reqwest::Client,
    pub(crate) params: SigV4Params,
}

impl AwsAdapter {
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
                service: "ec2".to_string(),
            },
        })
    }

    pub(crate) fn endpoint(&self) -> UnifiedResult<Url> {
        let url = Url::parse(&format!(
            "https://ec2.{}.amazonaws.com/",
            self.params.region
        ))
        .map_err(|_| UnifiedError::internal("Invalid AWS EC2 endpoint"))?;
        let host = url.host_str().unwrap_or("");
        if !host.ends_with(".amazonaws.com") {
            return Err(UnifiedError::bad_request("Invalid AWS EC2 endpoint host"));
        }
        Ok(url)
    }

    pub(crate) async fn describe_instances(&self, extra: &[(&str, &str)]) -> UnifiedResult<String> {
        let url = self.endpoint()?;
        let mut form = "Action=DescribeInstances&Version=2016-11-15".to_string();
        for (key, value) in extra {
            form.push_str(&format!(
                "&{}={}",
                url::form_urlencoded::byte_serialize(key.as_bytes()).collect::<String>(),
                url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>(),
            ));
        }
        let payload = form.as_bytes();
        // The date header value is filled by the signer; sign with the
        // real timestamp first.
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
                UnifiedError::internal("Computed AWS signature is not a valid header value")
            })?,
        );
        headers.insert(
            "x-amz-date",
            HeaderValue::from_str(&amz_date).map_err(|_| {
                UnifiedError::internal("Computed AWS date is not a valid header value")
            })?,
        );
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded; charset=utf-8"),
        );
        let request = self
            .client
            .post(url)
            .headers(headers)
            .body(form);
        send_text(&self.client, request, "aws").await
    }

    pub(crate) fn map_instance(block: &str) -> ServerInfo {
        let id = xml_tag_at(block, 0, "instanceId").unwrap_or_default();
        let state_pos = block.find("<instanceState>").unwrap_or(0);
        let raw_status = xml_tag_at(block, state_pos, "name").unwrap_or_default();
        let name = {
            let mut found = String::new();
            let mut from = 0;
            while let Some(key_pos) = block[from..].find("<key>Name</key>") {
                let abs = from + key_pos;
                if let Some(value) = xml_tag_at(block, abs, "value") {
                    found = value;
                    break;
                }
                from = abs + 1;
            }
            found
        };
        let zone_pos = block.find("<placement>").unwrap_or(0);
        let zone = xml_tag_at(block, zone_pos, "availabilityZone");
        ServerInfo {
            id: id.clone(),
            name: if name.is_empty() { id } else { name },
            item_kind: "server".to_string(),
            status: normalize_status(&raw_status).to_string(),
            raw_status: Some(raw_status),
            region: zone,
            public_ip: xml_tag_at(block, 0, "ipAddress"),
            private_ip: xml_tag_at(block, 0, "privateIpAddress"),
            cpu_cores: None,
            memory_mb: None,
            disk_gb: None,
            image: xml_tag_at(block, 0, "imageId"),
            created_at: None,
        }
    }

    pub(crate) fn parse_instances(xml: &str) -> Vec<ServerInfo> {
        // Each `<item>` inside `<instancesSet>` is one instance.
        let mut instances = Vec::new();
        for reservation in xml_blocks(xml, "reservationSet") {
            for set in xml_blocks(&reservation, "instancesSet") {
                for item in xml_blocks(&set, "item") {
                    if item.contains("<instanceId>") {
                        instances.push(Self::map_instance(&item));
                    }
                }
            }
        }
        instances
    }
}

#[async_trait]
impl CloudProvider for AwsAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Aws
    }

    async fn test_connection(&self) -> UnifiedResult<ProviderInfo> {
        let started = Instant::now();
        self.describe_instances(&[("MaxResults", "5")]).await?;
        Ok(ProviderInfo {
            kind: "aws".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: None,
            region: Some(self.params.region.clone()),
            detail: "AWS EC2 DescribeInstances reachable".to_string(),
        })
    }

    async fn list_servers(&self) -> UnifiedResult<Vec<ServerInfo>> {
        let xml = self
            .describe_instances(&[("MaxResults", &MAX_LIST_LIMIT.to_string())])
            .await?;
        Ok(Self::parse_instances(&xml))
    }

    async fn server_status(&self, id: &str) -> UnifiedResult<ServerInfo> {
        let xml = self
            .describe_instances(&[("InstanceIds.1", id)])
            .await?;
        Self::parse_instances(&xml)
            .into_iter()
            .find(|server| server.id == id)
            .ok_or_else(|| UnifiedError::not_found(format!("AWS instance \"{id}\" not found")))
    }
}

// ---------------------------------------------------------------------------
// Google Cloud Compute Engine adapter.
// ---------------------------------------------------------------------------

/// Google Cloud Compute Engine (`compute.googleapis.com`), OAuth2 bearer
/// token supplied through settings. List / status only in this round;
/// token refresh is out of scope -- an expired token fails the
/// connection test with a clear 401.
pub struct GoogleAdapter {
    pub(crate) client: reqwest::Client,
    pub(crate) access_token: String,
    pub(crate) project_id: String,
    pub(crate) zone: String,
}

impl GoogleAdapter {
    pub fn new(connection: &ProviderConnection) -> UnifiedResult<Self> {
        Ok(Self {
            client: build_client()?,
            access_token: connection
                .credentials
                .access_token
                .clone()
                .unwrap_or_default(),
            project_id: connection.project_id.clone().unwrap_or_default(),
            zone: connection.region.clone().unwrap_or_default(),
        })
    }

    pub(crate) async fn request(&self, method: Method, path: &str) -> UnifiedResult<Value> {
        let url = Url::parse(&format!("https://compute.googleapis.com{path}"))
            .map_err(|_| UnifiedError::internal("Invalid Google Compute API URL"))?;
        check_allowlist(&url, ProviderKind::Google)?;
        let headers = bearer_header(&self.access_token)?;
        let request = self.client.request(method, url).headers(headers);
        send_json(&self.client, request, "google").await
    }

    pub(crate) fn instances_path(&self) -> String {
        format!(
            "/compute/v1/projects/{}/zones/{}/instances",
            self.project_id, self.zone
        )
    }

    pub(crate) fn map_instance(value: &Value) -> ServerInfo {
        let raw_status = json_string(value, "status").unwrap_or("").to_string();
        let machine_type = json_string(value, "machineType")
            .and_then(|machine| machine.rsplit('/').next())
            .map(str::to_string);
        let mut public_ip = None;
        let mut private_ip = None;
        if let Some(interfaces) = value
            .get("networkInterfaces")
            .and_then(Value::as_array)
            .and_then(|interfaces| interfaces.first())
        {
            private_ip = json_string(interfaces, "networkIP").map(str::to_string);
            if let Some(configs) = interfaces
                .get("accessConfigs")
                .and_then(Value::as_array)
                .and_then(|configs| configs.first())
            {
                public_ip = json_string(configs, "natIP").map(str::to_string);
            }
        }
        let zone = json_string(value, "zone")
            .and_then(|zone| zone.rsplit('/').next())
            .map(str::to_string);
        ServerInfo {
            id: json_string(value, "name").unwrap_or("").to_string(),
            name: json_string(value, "name").unwrap_or("").to_string(),
            item_kind: "server".to_string(),
            status: normalize_status(&raw_status).to_string(),
            raw_status: Some(raw_status),
            region: zone,
            public_ip,
            private_ip,
            cpu_cores: None,
            memory_mb: None,
            disk_gb: None,
            image: machine_type,
            created_at: json_string(value, "creationTimestamp").map(str::to_string),
        }
    }
}

#[async_trait]
impl CloudProvider for GoogleAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Google
    }

    async fn test_connection(&self) -> UnifiedResult<ProviderInfo> {
        let started = Instant::now();
        self.request(Method::GET, &format!("{}?maxResults=1", self.instances_path()))
            .await?;
        Ok(ProviderInfo {
            kind: "google".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: Some(self.project_id.clone()),
            region: Some(self.zone.clone()),
            detail: "Google Cloud Compute Engine API reachable".to_string(),
        })
    }

    async fn list_servers(&self) -> UnifiedResult<Vec<ServerInfo>> {
        let body = self
            .request(
                Method::GET,
                &format!("{}?maxResults={MAX_LIST_LIMIT}", self.instances_path()),
            )
            .await?;
        let items = body
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(items.iter().map(Self::map_instance).collect())
    }

    async fn server_status(&self, id: &str) -> UnifiedResult<ServerInfo> {
        let body = self
            .request(Method::GET, &format!("{}/{}", self.instances_path(), id))
            .await?;
        Ok(Self::map_instance(&body))
    }
}

// ---------------------------------------------------------------------------
// Virtualizor adapter.
// ---------------------------------------------------------------------------

/// Virtualizor panel API. The panel URL is supplied on the connection
/// (`endpoint`); authentication is the API key (`api_token`) plus the
/// API password (`password`) sent as query parameters, as the panel
/// API specifies. List / status / create / start / stop / delete.
pub struct VirtualizorAdapter {
    pub(crate) client: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) api_key: String,
    pub(crate) api_pass: String,
}

impl VirtualizorAdapter {
    pub fn new(connection: &ProviderConnection) -> UnifiedResult<Self> {
        let endpoint = connection.endpoint.clone().unwrap_or_default();
        let url = Url::parse(endpoint.trim()).map_err(|_| {
            UnifiedError::bad_request("Invalid Virtualizor panel URL")
        })?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(UnifiedError::bad_request(
                "Virtualizor endpoint must use http or https",
            ));
        }
        Ok(Self {
            client: build_client()?,
            base_url: endpoint.trim_end_matches('/').to_string(),
            api_key: connection
                .credentials
                .api_token
                .clone()
                .unwrap_or_default(),
            api_pass: connection
                .credentials
                .password
                .clone()
                .unwrap_or_default(),
        })
    }

    /// Call one panel action. The panel answers JSON; an `error`
    /// object in the response is surfaced as a provider error.
    pub(crate) async fn call(
        &self,
        action: &str,
        extra: &[(&str, String)],
    ) -> UnifiedResult<Value> {
        let mut url = Url::parse(&format!("{}/index.php", self.base_url))
            .map_err(|_| UnifiedError::internal("Invalid Virtualizor panel URL"))?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("act", action);
            query.append_pair("api", "json");
            query.append_pair("apikey", &self.api_key);
            query.append_pair("apipass", &self.api_pass);
            for (key, value) in extra {
                query.append_pair(key, value);
            }
        }
        let request = self.client.get(url);
        let body = send_json(&self.client, request, "virtualizor").await?;
        if let Some(error) = body.get("error") {
            let message = error
                .as_str()
                .or_else(|| error.get("message").and_then(Value::as_str))
                .unwrap_or("Virtualizor API error");
            return Err(UnifiedError::internal(format!(
                "Virtualizor API error: {message}"
            )));
        }
        Ok(body)
    }

    pub(crate) fn map_vs(value: &Value) -> ServerInfo {
        let raw_status = json_string(value, "status").unwrap_or("").to_string();
        let id = value
            .get("vpsid")
            .or_else(|| value.get("id"))
            .map(|id| id.to_string())
            .unwrap_or_default();
        let ram = value
            .get("ram")
            .and_then(Value::as_u64)
            .or_else(|| {
                value
                    .get("ram")
                    .and_then(Value::as_str)
                    .and_then(|ram| ram.parse::<u64>().ok())
            });
        let disk = value
            .get("disk_space")
            .or_else(|| value.get("disk"))
            .and_then(Value::as_u64)
            .or_else(|| {
                value
                    .get("disk_space")
                    .and_then(Value::as_str)
                    .and_then(|disk| disk.parse::<u64>().ok())
            });
        ServerInfo {
            id,
            name: json_string(value, "hostname").unwrap_or("").to_string(),
            item_kind: "server".to_string(),
            status: normalize_status(&raw_status).to_string(),
            raw_status: Some(raw_status),
            region: None,
            public_ip: json_string(value, "ip").map(str::to_string),
            private_ip: None,
            cpu_cores: value
                .get("cores")
                .or_else(|| value.get("cpu"))
                .and_then(Value::as_u64)
                .map(|cores| cores as u32),
            memory_mb: ram,
            disk_gb: disk,
            image: json_string(value, "os_name")
                .or_else(|| json_string(value, "os"))
                .map(str::to_string),
            created_at: None,
        }
    }

    pub(crate) fn vs_list(body: &Value) -> Vec<Value> {
        // `listvs` answers either {"vs": [...]} or {"vs": {"1": {...}}}.
        match body.get("vs") {
            Some(Value::Array(items)) => items.clone(),
            Some(Value::Object(map)) => map.values().cloned().collect(),
            _ => Vec::new(),
        }
    }
}

#[async_trait]
impl CloudProvider for VirtualizorAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Virtualizor
    }

    async fn test_connection(&self) -> UnifiedResult<ProviderInfo> {
        let started = Instant::now();
        self.call("listvs", &[]).await?;
        Ok(ProviderInfo {
            kind: "virtualizor".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: None,
            region: None,
            detail: "Virtualizor panel API reachable".to_string(),
        })
    }

    async fn list_servers(&self) -> UnifiedResult<Vec<ServerInfo>> {
        let body = self.call("listvs", &[]).await?;
        Ok(Self::vs_list(&body)
            .iter()
            .map(Self::map_vs)
            .collect())
    }

    async fn server_status(&self, id: &str) -> UnifiedResult<ServerInfo> {
        // The panel has no cheap single-VPS status call that is stable
        // across versions, so status is read from the list and matched
        // by id. This is documented behaviour, not a shortcut taken
        // silently: the list call is the panel's canonical read.
        self.list_servers()
            .await?
            .into_iter()
            .find(|server| server.id == id)
            .ok_or_else(|| UnifiedError::not_found(format!("Virtualizor VPS \"{id}\" not found")))
    }

    async fn create_server(&self, request: &CreateServerRequest) -> UnifiedResult<ServerInfo> {
        if request.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Hostname is required"));
        }
        let os_id = request.image_id.clone().or_else(|| request.image.clone()).ok_or_else(|| {
            UnifiedError::bad_request("Virtualizor create requires image or image_id (the OS template id)")
        })?;
        let plan_id = request.plan_id.clone().ok_or_else(|| {
            UnifiedError::bad_request("Virtualizor create requires plan_id (the VPS plan id)")
        })?;
        let password = request.root_password.clone().ok_or_else(|| {
            UnifiedError::bad_request("Virtualizor create requires root_password")
        })?;
        let body = self
            .call(
                "addvs",
                &[
                    ("hostname", request.name.clone()),
                    ("user_pass", password),
                    ("osid", os_id),
                    ("plan", plan_id),
                ],
            )
            .await?;
        let vps_id = body
            .get("vpsid")
            .or_else(|| body.get("vsid"))
            .map(|id| id.to_string())
            .ok_or_else(|| {
                UnifiedError::internal("Virtualizor create succeeded but returned no VPS id")
            })?;
        self.server_status(&vps_id).await
    }

    async fn scale_server(
        &self,
        id: &str,
        request: &ScaleServerRequest,
    ) -> UnifiedResult<ServerInfo> {
        let mut extra: Vec<(&str, String)> = vec![("vpsid", id.to_string())];
        if let Some(ram) = request.memory_mb {
            extra.push(("ram", ram.to_string()));
        }
        if let Some(disk) = request.disk_gb {
            extra.push(("disk_space", disk.to_string()));
        }
        if let Some(cores) = request.cpu_cores {
            extra.push(("cores", cores.to_string()));
        }
        if extra.len() == 1 {
            return Err(UnifiedError::bad_request(
                "Virtualizor scale requires at least one of memory_mb, disk_gb, or cpu_cores",
            ));
        }
        self.call("editvs", &extra).await?;
        self.server_status(id).await
    }

    async fn delete_server(&self, id: &str) -> UnifiedResult<()> {
        self.call("deletevs", &[("vpsid", id.to_string())]).await?;
        Ok(())
    }

    async fn start_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        self.call("startvs", &[("vpsid", id.to_string())]).await?;
        self.server_status(id).await
    }

    async fn stop_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        self.call("stopvs", &[("vpsid", id.to_string())]).await?;
        self.server_status(id).await
    }
}

// ---------------------------------------------------------------------------
// Docker adapter.
// ---------------------------------------------------------------------------

/// Remote Docker Engine API (`/v1.44`), over TCP/TLS against the
/// endpoint on the connection. Containers list / inspect / start /
/// stop / delete. No image management and no container create in this
/// round. TLS verification stays on; a self-signed daemon needs a
/// properly configured CA.
pub struct DockerAdapter {
    pub(crate) client: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) bearer_token: Option<String>,
}

impl DockerAdapter {
    pub fn new(connection: &ProviderConnection) -> UnifiedResult<Self> {
        let endpoint = connection.endpoint.clone().unwrap_or_default();
        let url = Url::parse(endpoint.trim())
            .map_err(|_| UnifiedError::bad_request("Invalid Docker endpoint URL"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(UnifiedError::bad_request(
                "Docker endpoint must use http or https",
            ));
        }
        Ok(Self {
            client: build_client()?,
            base_url: endpoint.trim_end_matches('/').to_string(),
            bearer_token: connection
                .credentials
                .bearer_token
                .clone()
                .filter(|token| !token.trim().is_empty()),
        })
    }

    pub(crate) fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let mut request = self.client.request(
            method,
            format!("{}{path}", self.base_url),
        );
        if let Some(token) = &self.bearer_token {
            if let Ok(value) = HeaderValue::from_str(&format!("Bearer {token}")) {
                let mut headers = HeaderMap::new();
                headers.insert(AUTHORIZATION, value);
                request = request.headers(headers);
            }
        }
        request
    }

    pub(crate) fn map_container(value: &Value) -> ServerInfo {
        let raw_status = json_string(value, "State")
            .or_else(|| json_string(value, "Status"))
            .unwrap_or("")
            .to_string();
        let name = value
            .get("Names")
            .and_then(Value::as_array)
            .and_then(|names| names.first())
            .and_then(Value::as_str)
            .map(|name| name.trim_start_matches('/').to_string())
            .or_else(|| json_string(value, "Name").map(|name| name.trim_start_matches('/').to_string()))
            .unwrap_or_default();
        let id = json_string(value, "Id")
            .or_else(|| json_string(value, "ID"))
            .unwrap_or("")
            .to_string();
        let public_ip = value
            .get("Ports")
            .and_then(Value::as_array)
            .and_then(|ports| ports.first())
            .and_then(|port| json_string(port, "IP"))
            .map(str::to_string);
        ServerInfo {
            id,
            name,
            item_kind: "container".to_string(),
            status: normalize_status(&raw_status).to_string(),
            raw_status: Some(raw_status),
            region: None,
            public_ip,
            private_ip: None,
            cpu_cores: None,
            memory_mb: None,
            disk_gb: None,
            image: json_string(value, "Image").map(str::to_string),
            created_at: json_i64(value, "Created")
                .and_then(|created| {
                    DateTime::from_timestamp(created, 0).map(|time| time.to_rfc3339())
                }),
        }
    }

    pub(crate) fn map_inspect(value: &Value) -> ServerInfo {
        let state = value.get("State");
        let running = state.and_then(|state| state.get("Running")).and_then(Value::as_bool);
        let raw_status = state
            .and_then(|state| json_string(state, "Status"))
            .unwrap_or(if running == Some(true) { "running" } else { "exited" })
            .to_string();
        let networks = value
            .get("NetworkSettings")
            .and_then(|settings| settings.get("Networks"))
            .and_then(Value::as_object);
        let private_ip = networks.and_then(|networks| {
            networks.values().find_map(|network| {
                json_string(network, "IPAddress")
                    .filter(|ip| !ip.is_empty())
                    .map(str::to_string)
            })
        });
        let name = json_string(value, "Name")
            .map(|name| name.trim_start_matches('/').to_string())
            .unwrap_or_default();
        ServerInfo {
            id: json_string(value, "Id").unwrap_or("").to_string(),
            name,
            item_kind: "container".to_string(),
            status: normalize_status(&raw_status).to_string(),
            raw_status: Some(raw_status),
            region: None,
            public_ip: None,
            private_ip,
            cpu_cores: None,
            memory_mb: None,
            disk_gb: None,
            image: value
                .get("Config")
                .and_then(|config| json_string(config, "Image"))
                .map(str::to_string),
            created_at: json_string(value, "Created").map(str::to_string),
        }
    }
}

#[async_trait]
impl CloudProvider for DockerAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Docker
    }

    async fn test_connection(&self) -> UnifiedResult<ProviderInfo> {
        let started = Instant::now();
        let request = self.request(Method::GET, "/v1.44/version");
        let body = send_json(&self.client, request, "docker").await?;
        Ok(ProviderInfo {
            kind: "docker".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: json_string(&body, "Version").map(str::to_string),
            region: None,
            detail: "Docker Engine API reachable".to_string(),
        })
    }

    async fn list_servers(&self) -> UnifiedResult<Vec<ServerInfo>> {
        let request = self.request(Method::GET, "/v1.44/containers/json?all=1");
        let body = send_json(&self.client, request, "docker").await?;
        let items = body.as_array().cloned().unwrap_or_default();
        Ok(items.iter().map(Self::map_container).collect())
    }

    async fn server_status(&self, id: &str) -> UnifiedResult<ServerInfo> {
        let request = self.request(Method::GET, &format!("/v1.44/containers/{id}/json"));
        let body = send_json(&self.client, request, "docker").await?;
        Ok(Self::map_inspect(&body))
    }

    async fn delete_server(&self, id: &str) -> UnifiedResult<()> {
        let request = self.request(Method::DELETE, &format!("/v1.44/containers/{id}?force=false"));
        send_json(&self.client, request, "docker").await?;
        Ok(())
    }

    async fn start_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        let request = self.request(Method::POST, &format!("/v1.44/containers/{id}/start"));
        // Docker answers 204 No Content; send_json would fail parsing
        // an empty body, so handle the status directly.
        let response = request.send().await.map_err(|error| {
            UnifiedError::internal(format!("Docker start request failed: {error}"))
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(http_status_error(status, "docker", parse_retry_after(&response)));
        }
        self.server_status(id).await
    }

    async fn stop_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        let request = self.request(Method::POST, &format!("/v1.44/containers/{id}/stop?t=10"));
        let response = request.send().await.map_err(|error| {
            UnifiedError::internal(format!("Docker stop request failed: {error}"))
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(http_status_error(status, "docker", parse_retry_after(&response)));
        }
        self.server_status(id).await
    }
}

// ---------------------------------------------------------------------------
// Kubernetes adapter.
// ---------------------------------------------------------------------------

/// Kubernetes API server, service-account bearer token. Read-only in
/// this round: namespaces, pods, and deployments list / status.
pub struct KubernetesAdapter {
    pub(crate) client: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) bearer_token: String,
}

impl KubernetesAdapter {
    pub fn new(connection: &ProviderConnection) -> UnifiedResult<Self> {
        let endpoint = connection.endpoint.clone().unwrap_or_default();
        let url = Url::parse(endpoint.trim()).map_err(|_| {
            UnifiedError::bad_request("Invalid Kubernetes API server URL")
        })?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(UnifiedError::bad_request(
                "Kubernetes endpoint must use http or https",
            ));
        }
        Ok(Self {
            client: build_client()?,
            base_url: endpoint.trim_end_matches('/').to_string(),
            bearer_token: connection
                .credentials
                .bearer_token
                .clone()
                .unwrap_or_default(),
        })
    }

    pub(crate) async fn get(&self, path: &str) -> UnifiedResult<Value> {
        let url = Url::parse(&format!("{}{path}", self.base_url))
            .map_err(|_| UnifiedError::internal("Invalid Kubernetes API URL"))?;
        let headers = bearer_header(&self.bearer_token)?;
        let request = self.client.get(url).headers(headers);
        send_json(&self.client, request, "kubernetes").await
    }

    pub(crate) fn meta<'a>(value: &'a Value) -> (&'a str, &'a str, Option<&'a str>) {
        let metadata = value.get("metadata");
        let name = metadata
            .and_then(|meta| json_string(meta, "name"))
            .unwrap_or("");
        let namespace = metadata
            .and_then(|meta| json_string(meta, "namespace"))
            .unwrap_or("default");
        let created = metadata.and_then(|meta| json_string(meta, "creationTimestamp"));
        (name, namespace, created)
    }

    pub(crate) fn map_pod(value: &Value) -> ServerInfo {
        let (name, namespace, created) = Self::meta(value);
        let phase = value
            .get("status")
            .and_then(|status| json_string(status, "phase"))
            .unwrap_or("")
            .to_string();
        let image = value
            .get("spec")
            .and_then(|spec| spec.get("containers"))
            .and_then(Value::as_array)
            .and_then(|containers| containers.first())
            .and_then(|container| json_string(container, "image"))
            .map(str::to_string);
        ServerInfo {
            id: format!("pod/{namespace}/{name}"),
            name: name.to_string(),
            item_kind: "pod".to_string(),
            status: normalize_status(&phase).to_string(),
            raw_status: Some(phase),
            region: Some(namespace.to_string()),
            public_ip: None,
            private_ip: value
                .get("status")
                .and_then(|status| json_string(status, "podIP"))
                .map(str::to_string),
            cpu_cores: None,
            memory_mb: None,
            disk_gb: None,
            image,
            created_at: created.map(str::to_string),
        }
    }

    pub(crate) fn map_deployment(value: &Value) -> ServerInfo {
        let (name, namespace, created) = Self::meta(value);
        let status = value.get("status");
        let replicas = status.and_then(|status| json_u64(status, "replicas")).unwrap_or(0);
        let available = status
            .and_then(|status| json_u64(status, "availableReplicas"))
            .unwrap_or(0);
        let raw_status = if replicas == 0 {
            "stopped".to_string()
        } else if available >= replicas {
            "running".to_string()
        } else {
            "starting".to_string()
        };
        let image = value
            .get("spec")
            .and_then(|spec| spec.get("template"))
            .and_then(|template| template.get("spec"))
            .and_then(|spec| spec.get("containers"))
            .and_then(Value::as_array)
            .and_then(|containers| containers.first())
            .and_then(|container| json_string(container, "image"))
            .map(str::to_string);
        ServerInfo {
            id: format!("deployment/{namespace}/{name}"),
            name: name.to_string(),
            item_kind: "deployment".to_string(),
            status: normalize_status(&raw_status).to_string(),
            raw_status: Some(raw_status),
            region: Some(namespace.to_string()),
            public_ip: None,
            private_ip: None,
            cpu_cores: None,
            memory_mb: None,
            disk_gb: None,
            image,
            created_at: created.map(str::to_string),
        }
    }
}

#[async_trait]
impl CloudProvider for KubernetesAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Kubernetes
    }

    async fn test_connection(&self) -> UnifiedResult<ProviderInfo> {
        let started = Instant::now();
        self.get("/api/v1/namespaces?limit=1").await?;
        Ok(ProviderInfo {
            kind: "kubernetes".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: None,
            region: None,
            detail: "Kubernetes API server reachable".to_string(),
        })
    }

    async fn list_servers(&self) -> UnifiedResult<Vec<ServerInfo>> {
        let mut items = Vec::new();
        let pods = self.get("/api/v1/pods?limit=200").await?;
        if let Some(list) = pods.get("items").and_then(Value::as_array) {
            items.extend(list.iter().map(Self::map_pod));
        }
        let deployments = self.get("/apis/apps/v1/deployments?limit=200").await?;
        if let Some(list) = deployments.get("items").and_then(Value::as_array) {
            items.extend(list.iter().map(Self::map_deployment));
        }
        Ok(items)
    }

    async fn server_status(&self, id: &str) -> UnifiedResult<ServerInfo> {
        // Ids are `pod/<namespace>/<name>` or
        // `deployment/<namespace>/<name>` (see `list_servers`).
        let parts: Vec<&str> = id.split('/').collect();
        if parts.len() != 3 {
            return Err(UnifiedError::bad_request(
                "Kubernetes resource id must look like \"pod/<namespace>/<name>\" or \"deployment/<namespace>/<name>\"",
            ));
        }
        let (item_kind, namespace, name) = (parts[0], parts[1], parts[2]);
        match item_kind {
            "pod" => {
                let body = self
                    .get(&format!("/api/v1/namespaces/{namespace}/pods/{name}"))
                    .await?;
                Ok(Self::map_pod(&body))
            }
            "deployment" => {
                let body = self
                    .get(&format!(
                        "/apis/apps/v1/namespaces/{namespace}/deployments/{name}"
                    ))
                    .await?;
                Ok(Self::map_deployment(&body))
            }
            _ => Err(UnifiedError::bad_request(format!(
                "Unknown Kubernetes resource kind \"{item_kind}\""
            ))),
        }
    }
}
