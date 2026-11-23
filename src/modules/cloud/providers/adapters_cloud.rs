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

    async fn suspend_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        self.call("suspendvs", &[("vpsid", id.to_string())]).await?;
        self.server_status(id).await
    }

    async fn unsuspend_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        self.call("unsuspendvs", &[("vpsid", id.to_string())]).await?;
        self.server_status(id).await
    }

    async fn change_password(
        &self,
        id: &str,
        request: &ChangePasswordRequest,
    ) -> UnifiedResult<ServerInfo> {
        if request.new_password.trim().is_empty() {
            return Err(UnifiedError::bad_request("New password is required"));
        }
        self.call(
            "changepassword",
            &[
                ("vpsid", id.to_string()),
                ("newpass", request.new_password.clone()),
            ],
        )
        .await?;
        self.server_status(id).await
    }
}

// ---------------------------------------------------------------------------
// Docker adapter.
// ---------------------------------------------------------------------------

/// Remote Docker Engine API (`/v1.44`), over TCP/TLS against the
/// endpoint on the connection. Containers list / inspect / create /
/// start / stop / pause / unpause / delete, with one-shot stats.
/// No image management: the image must exist on the host or be
/// pullable by name. TLS verification stays on; a self-signed daemon
/// needs a properly configured CA.
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

    fn metadata(&self) -> ProviderMetadata {
        metadata_with_version(self.kind(), "v1.44")
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

    async fn create_server(&self, request: &CreateServerRequest) -> UnifiedResult<ServerInfo> {
        let name = request.name.trim();
        if name.is_empty()
            || !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' || ch == '-')
            || !name
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_alphanumeric())
        {
            return Err(UnifiedError::bad_request(
                "Docker container name must start with a letter or digit and contain only letters, digits, '_' , '.' , or '-'",
            ));
        }
        let image = request
            .image
            .clone()
            .or_else(|| request.image_id.clone())
            .filter(|image| !image.trim().is_empty())
            .ok_or_else(|| UnifiedError::bad_request("Docker create requires an image"))?;
        let mut body = serde_json::json!({ "Image": image });
        if let Some(command) = &request.command {
            if !command.is_empty() {
                body["Cmd"] = serde_json::json!(command);
            }
        }
        if let Some(env) = &request.env {
            if !env.is_empty() {
                let vars: Vec<String> = env
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect();
                body["Env"] = serde_json::json!(vars);
            }
        }
        let mut host_config = serde_json::json!({});
        if let Some(ports) = &request.ports {
            if !ports.is_empty() {
                let mut exposed = serde_json::Map::new();
                let mut bindings = serde_json::Map::new();
                for port in ports {
                    let protocol = port
                        .protocol
                        .clone()
                        .unwrap_or_else(|| "tcp".to_string())
                        .to_ascii_lowercase();
                    if protocol != "tcp" && protocol != "udp" {
                        return Err(UnifiedError::bad_request(
                            "Docker port protocol must be \"tcp\" or \"udp\"",
                        ));
                    }
                    let key = format!("{}/{protocol}", port.container_port);
                    exposed.insert(key.clone(), serde_json::json!({}));
                    if let Some(host_port) = port.host_port {
                        bindings.insert(
                            key,
                            serde_json::json!([{ "HostPort": host_port.to_string() }]),
                        );
                    }
                }
                body["ExposedPorts"] = serde_json::Value::Object(exposed);
                host_config["PortBindings"] = serde_json::Value::Object(bindings);
            }
        }
        if let Some(memory_mb) = request.memory_mb {
            host_config["Memory"] = serde_json::json!(memory_mb.saturating_mul(1024 * 1024));
        }
        if let Some(cpu_cores) = request.cpu_cores {
            host_config["NanoCpus"] = serde_json::json!(i64::from(cpu_cores).saturating_mul(1_000_000_000));
        }
        body["HostConfig"] = host_config;
        let req = self
            .request(Method::POST, "/v1.44/containers/create")
            .query(&[("name", name)])
            .json(&body);
        let created = send_json(&self.client, req, "docker").await?;
        let id = json_string(&created, "Id")
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                UnifiedError::internal("Docker create succeeded but returned no container id")
            })?
            .to_string();
        self.server_status(&id).await
    }

    async fn suspend_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        // Pause freezes the container's processes; the inspect status
        // reads "paused" (normalized to "stopped", raw "paused").
        let request = self.request(Method::POST, &format!("/v1.44/containers/{id}/pause"));
        let response = request.send().await.map_err(|error| {
            UnifiedError::internal(format!("Docker pause request failed: {error}"))
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(http_status_error(status, "docker", parse_retry_after(&response)));
        }
        self.server_status(id).await
    }

    async fn unsuspend_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        let request = self.request(Method::POST, &format!("/v1.44/containers/{id}/unpause"));
        let response = request.send().await.map_err(|error| {
            UnifiedError::internal(format!("Docker unpause request failed: {error}"))
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(http_status_error(status, "docker", parse_retry_after(&response)));
        }
        self.server_status(id).await
    }

    async fn usage_metrics(&self, id: &str) -> UnifiedResult<UsageMetrics> {
        let request = self.request(
            Method::GET,
            &format!("/v1.44/containers/{id}/stats?stream=false"),
        );
        let body = send_json(&self.client, request, "docker").await?;
        let cpu = body.get("cpu_stats");
        let precpu = body.get("precpu_stats");
        let cpu_delta = cpu
            .and_then(|stats| stats.get("cpu_usage"))
            .and_then(|usage| usage.get("total_usage"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .saturating_sub(
                precpu
                    .and_then(|stats| stats.get("cpu_usage"))
                    .and_then(|usage| usage.get("total_usage"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            ) as f64;
        let system_delta = cpu
            .and_then(|stats| stats.get("system_cpu_usage"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .saturating_sub(
                precpu
                    .and_then(|stats| stats.get("system_cpu_usage"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            ) as f64;
        let online_cpus = cpu
            .and_then(|stats| stats.get("online_cpus"))
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                cpu.and_then(|stats| stats.get("cpu_usage"))
                    .and_then(|usage| usage.get("percpu_usage"))
                    .and_then(Value::as_array)
                    .map(|cores| cores.len() as u64)
                    .unwrap_or(1)
            })
            .max(1) as f64;
        let cpu_percent = if system_delta > 0.0 {
            Some(cpu_delta / system_delta * online_cpus * 100.0)
        } else {
            None
        };
        let memory_mb = body
            .get("memory_stats")
            .and_then(|stats| stats.get("usage"))
            .and_then(Value::as_u64)
            .map(|bytes| bytes / (1024 * 1024));
        let (rx_bytes, tx_bytes) = body
            .get("networks")
            .and_then(Value::as_object)
            .map(|networks| {
                networks.values().fold((0u64, 0u64), |(rx, tx), network| {
                    (
                        rx.saturating_add(
                            network.get("rx_bytes").and_then(Value::as_u64).unwrap_or(0),
                        ),
                        tx.saturating_add(
                            network.get("tx_bytes").and_then(Value::as_u64).unwrap_or(0),
                        ),
                    )
                })
            })
            .unwrap_or((0, 0));
        Ok(UsageMetrics {
            server_id: id.to_string(),
            cpu_percent,
            memory_mb,
            disk_gb: None,
            bandwidth_in_mb: Some(rx_bytes as f64 / (1024.0 * 1024.0)),
            bandwidth_out_mb: Some(tx_bytes as f64 / (1024.0 * 1024.0)),
            sampled_at: body
                .get("read")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| Utc::now().to_rfc3339()),
        })
    }
}

// ---------------------------------------------------------------------------
// Kubernetes adapter.
// ---------------------------------------------------------------------------

/// Kubernetes API server, service-account bearer token. Namespaces,
/// pods, and deployments list / status, plus deployment create /
/// scale / delete (suspend scales replicas to 0, unsuspend back to 1).
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

    /// Send a JSON write request (POST / PATCH / DELETE). The scale
    /// subresource needs a merge-patch content type, so callers pass
    /// it explicitly through `request_json_typed`.
    pub(crate) async fn request_json(
        &self,
        method: Method,
        path: &str,
        body: &Value,
    ) -> UnifiedResult<Value> {
        self.request_json_typed(method, path, body, "application/json")
            .await
    }

    pub(crate) async fn request_json_typed(
        &self,
        method: Method,
        path: &str,
        body: &Value,
        content_type: &str,
    ) -> UnifiedResult<Value> {
        let url = Url::parse(&format!("{}{path}", self.base_url))
            .map_err(|_| UnifiedError::internal("Invalid Kubernetes API URL"))?;
        let mut headers = bearer_header(&self.bearer_token)?;
        let content_type = HeaderValue::from_str(content_type)
            .map_err(|_| UnifiedError::internal("Invalid Kubernetes content type"))?;
        headers.insert(CONTENT_TYPE, content_type);
        let request = self.client.request(method, url).headers(headers).json(body);
        send_json_or_empty(&self.client, request, "kubernetes").await
    }

    /// Split `pod/<namespace>/<name>` or `deployment/<namespace>/<name>`.
    fn split_resource_id(id: &str) -> UnifiedResult<(&str, &str, &str)> {
        let parts: Vec<&str> = id.split('/').collect();
        if parts.len() != 3 || (parts[0] != "pod" && parts[0] != "deployment") {
            return Err(UnifiedError::bad_request(
                "Kubernetes resource id must look like \"pod/<namespace>/<name>\" or \"deployment/<namespace>/<name>\"",
            ));
        }
        Ok((parts[0], parts[1], parts[2]))
    }

    /// Validate a Kubernetes resource name (DNS-1123 subdomain).
    fn validate_resource_name(name: &str) -> UnifiedResult<()> {
        if name.is_empty()
            || name.len() > 253
            || !name.chars().all(|ch| {
                ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '.'
            })
            || !name
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_alphanumeric())
            || !name
                .chars()
                .last()
                .is_some_and(|ch| ch.is_ascii_alphanumeric())
        {
            return Err(UnifiedError::bad_request(
                "Kubernetes resource name must be a DNS-1123 subdomain (lowercase letters, digits, '-' and '.')",
            ));
        }
        Ok(())
    }

    /// Scale one deployment through the `/scale` subresource.
    async fn scale_deployment(&self, namespace: &str, name: &str, replicas: u32) -> UnifiedResult<()> {
        self.request_json_typed(
            Method::PATCH,
            &format!("/apis/apps/v1/namespaces/{namespace}/deployments/{name}/scale"),
            &serde_json::json!({ "spec": { "replicas": replicas } }),
            "application/merge-patch+json",
        )
        .await?;
        Ok(())
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

    fn metadata(&self) -> ProviderMetadata {
        metadata_with_version(self.kind(), "apps/v1")
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
        let (item_kind, namespace, name) = Self::split_resource_id(id)?;
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
            _ => unreachable!("split_resource_id only accepts pod and deployment"),
        }
    }

    async fn create_server(&self, request: &CreateServerRequest) -> UnifiedResult<ServerInfo> {
        let name = request.name.trim();
        Self::validate_resource_name(name)?;
        let image = request
            .image
            .clone()
            .or_else(|| request.image_id.clone())
            .filter(|image| !image.trim().is_empty())
            .ok_or_else(|| UnifiedError::bad_request("Kubernetes create requires an image"))?;
        let namespace = request
            .namespace
            .clone()
            .unwrap_or_else(|| "default".to_string());
        Self::validate_resource_name(&namespace)?;
        let replicas = request.replicas.unwrap_or(1);
        let mut container = serde_json::json!({
            "name": name,
            "image": image,
        });
        if let Some(env) = &request.env {
            if !env.is_empty() {
                let vars: Vec<Value> = env
                    .iter()
                    .map(|(key, value)| serde_json::json!({ "name": key, "value": value }))
                    .collect();
                container["env"] = serde_json::Value::Array(vars);
            }
        }
        if let Some(ports) = &request.ports {
            if !ports.is_empty() {
                let mut mapped: Vec<Value> = Vec::new();
                for port in ports {
                    let protocol = port
                        .protocol
                        .clone()
                        .unwrap_or_else(|| "TCP".to_string())
                        .to_ascii_uppercase();
                    if protocol != "TCP" && protocol != "UDP" {
                        return Err(UnifiedError::bad_request(
                            "Kubernetes port protocol must be \"TCP\" or \"UDP\"",
                        ));
                    }
                    mapped.push(serde_json::json!({
                        "containerPort": port.container_port,
                        "protocol": protocol,
                    }));
                }
                container["ports"] = serde_json::Value::Array(mapped);
            }
        }
        if let Some(command) = &request.command {
            if !command.is_empty() {
                container["command"] = serde_json::json!(command);
            }
        }
        // Resource limits from the request (the app flow fills them
        // from the plan quotas).
        if request.memory_mb.is_some() || request.cpu_cores.is_some() {
            let mut limits = serde_json::Map::new();
            if let Some(memory_mb) = request.memory_mb {
                limits.insert(
                    "memory".to_string(),
                    serde_json::json!(format!("{memory_mb}Mi")),
                );
            }
            if let Some(cpu_cores) = request.cpu_cores {
                limits.insert("cpu".to_string(), serde_json::json!(cpu_cores.to_string()));
            }
            container["resources"] = serde_json::json!({ "limits": limits });
        }
        let manifest = serde_json::json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": {
                "name": name,
                "namespace": namespace,
                "labels": { "app": name },
            },
            "spec": {
                "replicas": replicas,
                "selector": { "matchLabels": { "app": name } },
                "template": {
                    "metadata": { "labels": { "app": name } },
                    "spec": { "containers": [container] },
                },
            },
        });
        self.request_json(
            Method::POST,
            &format!("/apis/apps/v1/namespaces/{namespace}/deployments"),
            &manifest,
        )
        .await?;
        self.server_status(&format!("deployment/{namespace}/{name}"))
            .await
    }

    async fn scale_server(
        &self,
        id: &str,
        request: &ScaleServerRequest,
    ) -> UnifiedResult<ServerInfo> {
        let (item_kind, namespace, name) = Self::split_resource_id(id)?;
        if item_kind != "deployment" {
            return Err(UnifiedError::bad_request(
                "Kubernetes scale applies to deployments, not pods",
            ));
        }
        let replicas = request.replicas.ok_or_else(|| {
            UnifiedError::bad_request("Kubernetes scale requires replicas")
        })?;
        self.scale_deployment(&namespace, &name, replicas).await?;
        self.server_status(id).await
    }

    async fn delete_server(&self, id: &str) -> UnifiedResult<()> {
        let (item_kind, namespace, name) = Self::split_resource_id(id)?;
        let path = match item_kind {
            "pod" => format!("/api/v1/namespaces/{namespace}/pods/{name}"),
            _ => format!("/apis/apps/v1/namespaces/{namespace}/deployments/{name}"),
        };
        self.request_json(Method::DELETE, &path, &serde_json::json!({}))
            .await?;
        Ok(())
    }

    async fn suspend_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        // Suspend is scale-to-zero; the deployment keeps its pod spec,
        // so unsuspend restores it by scaling back to 1. The
        // pre-suspend replica count is not preserved in this round.
        let (item_kind, namespace, name) = Self::split_resource_id(id)?;
        if item_kind != "deployment" {
            return Err(UnifiedError::bad_request(
                "Kubernetes suspend applies to deployments, not pods; deleting a pod restarts it",
            ));
        }
        self.scale_deployment(&namespace, &name, 0).await?;
        self.server_status(id).await
    }

    async fn unsuspend_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        let (item_kind, namespace, name) = Self::split_resource_id(id)?;
        if item_kind != "deployment" {
            return Err(UnifiedError::bad_request(
                "Kubernetes unsuspend applies to deployments, not pods",
            ));
        }
        self.scale_deployment(&namespace, &name, 1).await?;
        self.server_status(id).await
    }
}
