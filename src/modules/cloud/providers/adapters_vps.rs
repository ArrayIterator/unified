// providers module -- Provider adapters: DigitalOcean and Linode.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use async_trait::async_trait;
use reqwest::Method;
use serde_json::Value;
use std::time::Instant;
use url::Url;

use super::*;

// DigitalOcean adapter.

/// DigitalOcean API (`api.digitalocean.com`), bearer token auth.
/// Droplets list (paginated, up to 5 pages of 200) / status /
/// create / resize / delete / power actions.
pub struct DigitalOceanAdapter {
    pub(crate) client: reqwest::Client,
    pub(crate) token: String,
}

impl DigitalOceanAdapter {
    pub fn new(connection: &ProviderConnection) -> UnifiedResult<Self> {
        let token = connection
            .credentials
            .api_token
            .clone()
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| {
                UnifiedError::bad_request("DigitalOcean connection requires api_token")
            })?;
        Ok(Self {
            client: build_client()?,
            token,
        })
    }

    pub(crate) fn builder(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> UnifiedResult<reqwest::RequestBuilder> {
        let url = Url::parse(&format!("https://api.digitalocean.com/v2{path}"))
            .map_err(|_| UnifiedError::internal("Invalid DigitalOcean API URL"))?;
        check_allowlist(&url, ProviderKind::DigitalOcean)?;
        let mut request = self.client.request(method, url);
        request = request.headers(bearer_header(&self.token)?);
        if let Some(body) = body {
            request = request.json(&body);
        }
        Ok(request)
    }

    pub(crate) async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> UnifiedResult<Value> {
        let request = self.builder(method, path, body)?;
        send_json(&self.client, request, "digitalocean").await
    }

    pub(crate) fn map_droplet(value: &Value) -> ServerInfo {
        let id = value
            .get("id")
            .map(|id| id.to_string())
            .unwrap_or_default();
        let raw_status = json_string(value, "status").unwrap_or("").to_string();
        let region = value
            .get("region")
            .and_then(|region| json_string(region, "slug"))
            .map(str::to_string);
        let mut public_ip = None;
        let mut private_ip = None;
        if let Some(networks) = value.get("networks").and_then(|net| net.get("v4")) {
            if let Some(addresses) = networks.as_array() {
                for address in addresses {
                    let ip = json_string(address, "ip_address").map(str::to_string);
                    match json_string(address, "type") {
                        Some("public") if public_ip.is_none() => public_ip = ip,
                        Some("private") if private_ip.is_none() => private_ip = ip,
                        _ => {}
                    }
                }
            }
        }
        let size = value.get("size");
        ServerInfo {
            id,
            name: json_string(value, "name").unwrap_or("").to_string(),
            item_kind: "server".to_string(),
            status: normalize_status(&raw_status).to_string(),
            raw_status: Some(raw_status),
            region,
            public_ip,
            private_ip,
            cpu_cores: size.and_then(|size| json_u64(size, "vcpus")).map(|cores| cores as u32),
            memory_mb: size.and_then(|size| json_u64(size, "memory")),
            disk_gb: size.and_then(|size| json_u64(size, "disk")),
            image: value
                .get("image")
                .and_then(|image| {
                    json_string(image, "name").or_else(|| json_string(image, "slug"))
                })
                .map(str::to_string),
            created_at: json_string(value, "created_at").map(str::to_string),
        }
    }
}

#[async_trait]
impl CloudProvider for DigitalOceanAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::DigitalOcean
    }

    fn metadata(&self) -> ProviderMetadata {
        metadata_with_version(self.kind(), "v2")
    }

    async fn test_connection(&self) -> UnifiedResult<ProviderInfo> {
        let started = Instant::now();
        let body = self.request(Method::GET, "/account", None).await?;
        Ok(ProviderInfo {
            kind: "digitalocean".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: body
                .get("account")
                .and_then(|account| json_string(account, "email"))
                .map(str::to_string),
            region: None,
            detail: "DigitalOcean API reachable".to_string(),
        })
    }

    async fn list_servers(&self) -> UnifiedResult<Vec<ServerInfo>> {
        // Follow the `links.pages.next` marker, up to 5 pages, so a
        // large account is not silently truncated at one page.
        let mut servers = Vec::new();
        let mut page = 1;
        loop {
            let body = self
                .request(
                    Method::GET,
                    &format!("/droplets?per_page={MAX_LIST_LIMIT}&page={page}"),
                    None,
                )
                .await?;
            let droplets = body
                .get("droplets")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            servers.extend(droplets.iter().map(Self::map_droplet));
            let has_next = body
                .get("links")
                .and_then(|links| links.get("pages"))
                .and_then(|pages| pages.get("next"))
                .is_some();
            if !has_next || page >= 5 {
                break;
            }
            page += 1;
        }
        Ok(servers)
    }

    async fn server_status(&self, id: &str) -> UnifiedResult<ServerInfo> {
        if id.trim().is_empty() || id.parse::<u64>().is_err() {
            return Err(UnifiedError::bad_request(
                "DigitalOcean droplet id must be numeric",
            ));
        }
        let body = self
            .request(Method::GET, &format!("/droplets/{id}"), None)
            .await?;
        body.get("droplet")
            .map(Self::map_droplet)
            .ok_or_else(|| {
                UnifiedError::not_found(format!("DigitalOcean droplet \"{id}\" not found"))
            })
    }

    async fn create_server(&self, request: &CreateServerRequest) -> UnifiedResult<ServerInfo> {
        if request.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Server name is required"));
        }
        let region = request.region.clone().ok_or_else(|| {
            UnifiedError::bad_request("DigitalOcean create requires region (for example \"ams3\")")
        })?;
        let size = request.server_type.clone().ok_or_else(|| {
            UnifiedError::bad_request("DigitalOcean create requires server_type (the droplet size slug, for example \"s-1vcpu-1gb\")")
        })?;
        let image = request
            .image_id
            .clone()
            .or_else(|| request.image.clone())
            .ok_or_else(|| {
                UnifiedError::bad_request("DigitalOcean create requires image or image_id")
            })?;
        let mut payload = serde_json::json!({
            "name": request.name,
            "region": region,
            "size": size,
            "image": image,
        });
        if let Some(keys) = &request.ssh_key_ids {
            payload["ssh_keys"] = Value::Array(
                keys.iter().map(|key| Value::String(key.clone())).collect(),
            );
        }
        let body = self
            .request(Method::POST, "/droplets", Some(payload))
            .await?;
        body.get("droplet")
            .map(Self::map_droplet)
            .ok_or_else(|| {
                UnifiedError::internal("DigitalOcean create response has no droplet object")
            })
    }

    async fn scale_server(
        &self,
        id: &str,
        request: &ScaleServerRequest,
    ) -> UnifiedResult<ServerInfo> {
        let size = request.server_type.clone().ok_or_else(|| {
            UnifiedError::bad_request("DigitalOcean scale requires server_type (the new droplet size slug)")
        })?;
        // `disk: false` resizes CPU / RAM only. Growing the disk
        // needs a powered-off droplet and is irreversible, so it is
        // deliberately not done here.
        self.request(
            Method::POST,
            &format!("/droplets/{id}/actions"),
            Some(serde_json::json!({
                "type": "resize",
                "size": size,
                "disk": false,
            })),
        )
        .await?;
        self.server_status(id).await
    }

    async fn delete_server(&self, id: &str) -> UnifiedResult<()> {
        let request = self.builder(Method::DELETE, &format!("/droplets/{id}"), None)?;
        send_no_content(&self.client, request, "digitalocean").await
    }

    async fn start_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        self.request(
            Method::POST,
            &format!("/droplets/{id}/actions"),
            Some(serde_json::json!({ "type": "power_on" })),
        )
        .await?;
        self.server_status(id).await
    }

    async fn stop_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        self.request(
            Method::POST,
            &format!("/droplets/{id}/actions"),
            Some(serde_json::json!({ "type": "power_off" })),
        )
        .await?;
        self.server_status(id).await
    }
}

// Linode adapter.

/// Linode API (`api.linode.com`), bearer token auth. Instances list
/// (paginated, up to 5 pages of 200) / status / create / resize /
/// delete / boot / shutdown.
pub struct LinodeAdapter {
    pub(crate) client: reqwest::Client,
    pub(crate) token: String,
}

impl LinodeAdapter {
    pub fn new(connection: &ProviderConnection) -> UnifiedResult<Self> {
        let token = connection
            .credentials
            .api_token
            .clone()
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| UnifiedError::bad_request("Linode connection requires api_token"))?;
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
        let url = Url::parse(&format!("https://api.linode.com/v4{path}"))
            .map_err(|_| UnifiedError::internal("Invalid Linode API URL"))?;
        check_allowlist(&url, ProviderKind::Linode)?;
        let mut request = self.client.request(method, url);
        request = request.headers(bearer_header(&self.token)?);
        if let Some(body) = body {
            request = request.json(&body);
        }
        send_json(&self.client, request, "linode").await
    }

    pub(crate) fn map_instance(value: &Value) -> ServerInfo {
        let id = value
            .get("id")
            .map(|id| id.to_string())
            .unwrap_or_default();
        let raw_status = json_string(value, "status").unwrap_or("").to_string();
        let public_ip = value
            .get("ipv4")
            .and_then(Value::as_array)
            .and_then(|ips| ips.first())
            .and_then(Value::as_str)
            .map(str::to_string);
        let specs = value.get("specs");
        ServerInfo {
            id,
            name: json_string(value, "label").unwrap_or("").to_string(),
            item_kind: "server".to_string(),
            status: normalize_status(&raw_status).to_string(),
            raw_status: Some(raw_status),
            region: json_string(value, "region").map(str::to_string),
            public_ip,
            private_ip: None,
            cpu_cores: specs.and_then(|specs| json_u64(specs, "vcpus")).map(|cores| cores as u32),
            memory_mb: specs.and_then(|specs| json_u64(specs, "memory")),
            disk_gb: specs.and_then(|specs| json_u64(specs, "disk")).map(|mb| mb / 1024),
            image: json_string(value, "image").map(str::to_string),
            created_at: json_string(value, "created").map(str::to_string),
        }
    }
}

#[async_trait]
impl CloudProvider for LinodeAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Linode
    }

    fn metadata(&self) -> ProviderMetadata {
        metadata_with_version(self.kind(), "v4")
    }

    async fn test_connection(&self) -> UnifiedResult<ProviderInfo> {
        let started = Instant::now();
        let body = self.request(Method::GET, "/account", None).await?;
        Ok(ProviderInfo {
            kind: "linode".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: json_string(&body, "email").map(str::to_string),
            region: None,
            detail: "Linode API reachable".to_string(),
        })
    }

    async fn list_servers(&self) -> UnifiedResult<Vec<ServerInfo>> {
        let mut servers = Vec::new();
        let mut page = 1;
        loop {
            let body = self
                .request(
                    Method::GET,
                    &format!("/linode/instances?page_size={MAX_LIST_LIMIT}&page={page}"),
                    None,
                )
                .await?;
            let instances = body
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            servers.extend(instances.iter().map(Self::map_instance));
            let total_pages = body.get("pages").and_then(Value::as_u64).unwrap_or(1);
            if page >= total_pages || page >= 5 {
                break;
            }
            page += 1;
        }
        Ok(servers)
    }

    async fn server_status(&self, id: &str) -> UnifiedResult<ServerInfo> {
        if id.trim().is_empty() || id.parse::<u64>().is_err() {
            return Err(UnifiedError::bad_request(
                "Linode instance id must be numeric",
            ));
        }
        self.request(Method::GET, &format!("/linode/instances/{id}"), None)
            .await
            .map(|body| Self::map_instance(&body))
    }

    async fn create_server(&self, request: &CreateServerRequest) -> UnifiedResult<ServerInfo> {
        if request.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Server name is required"));
        }
        let region = request.region.clone().ok_or_else(|| {
            UnifiedError::bad_request("Linode create requires region (for example \"eu-west\")")
        })?;
        let instance_type = request.server_type.clone().ok_or_else(|| {
            UnifiedError::bad_request("Linode create requires server_type (the plan id, for example \"g6-standard-1\")")
        })?;
        let image = request
            .image_id
            .clone()
            .or_else(|| request.image.clone())
            .ok_or_else(|| {
                UnifiedError::bad_request("Linode create requires image or image_id")
            })?;
        // Linode requires a root password or authorized users for a
        // create; this adapter requires the root password.
        let root_password = request.root_password.clone().ok_or_else(|| {
            UnifiedError::bad_request("Linode create requires root_password")
        })?;
        let body = self
            .request(
                Method::POST,
                "/linode/instances",
                Some(serde_json::json!({
                    "label": request.name,
                    "region": region,
                    "type": instance_type,
                    "image": image,
                    "root_pass": root_password,
                })),
            )
            .await?;
        Ok(Self::map_instance(&body))
    }

    async fn scale_server(
        &self,
        id: &str,
        request: &ScaleServerRequest,
    ) -> UnifiedResult<ServerInfo> {
        let instance_type = request.server_type.clone().ok_or_else(|| {
            UnifiedError::bad_request("Linode scale requires server_type (the new plan id)")
        })?;
        // Linode rejects a resize of a running instance; stop it
        // first. The resize itself is queued by the API.
        self.request(
            Method::POST,
            &format!("/linode/instances/{id}/resize"),
            Some(serde_json::json!({ "type": instance_type })),
        )
        .await?;
        self.server_status(id).await
    }

    async fn delete_server(&self, id: &str) -> UnifiedResult<()> {
        self.request(Method::DELETE, &format!("/linode/instances/{id}"), None)
            .await?;
        Ok(())
    }

    async fn change_password(
        &self,
        id: &str,
        request: &ChangePasswordRequest,
    ) -> UnifiedResult<ServerInfo> {
        if request.new_password.trim().is_empty() {
            return Err(UnifiedError::bad_request("New password is required"));
        }
        self.request(
            Method::POST,
            &format!("/linode/instances/{id}/password"),
            Some(serde_json::json!({ "root_pass": request.new_password })),
        )
        .await?;
        self.server_status(id).await
    }

    async fn start_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        self.request(
            Method::POST,
            &format!("/linode/instances/{id}/boot"),
            Some(serde_json::json!({})),
        )
        .await?;
        self.server_status(id).await
    }

    async fn stop_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        self.request(
            Method::POST,
            &format!("/linode/instances/{id}/shutdown"),
            Some(serde_json::json!({})),
        )
        .await?;
        self.server_status(id).await
    }
}
