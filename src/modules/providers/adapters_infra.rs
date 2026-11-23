// providers module -- Provider adapters: OpenStack, Proxmox, KVM, and Xen, plus XML-RPC helpers.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use reqwest::Method;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Instant;
use url::Url;

use super::*;

// ---------------------------------------------------------------------------
// OpenStack adapter.
// ---------------------------------------------------------------------------

/// OpenStack (Keystone v3 + Nova) against a user-supplied cloud
/// endpoint. Servers list / status / create / delete / start / stop.
///
/// Auth: password grant against `{endpoint}/v3/auth/tokens`; the
/// token and the public compute endpoint are taken from the Keystone
/// response and the service catalog. The project (`project_id`) is
/// scoped by name in the `Default` domain; a deployment whose
/// project lives in another domain cannot use this adapter as-is.
///
/// A fresh token is fetched for every operation: adapters are
/// short-lived here (built per request), so there is no token cache
/// in this round. Resize is not supported: Nova resize needs a
/// separate confirm step after the resize completes, which would
/// need polling that this round does not implement.
pub struct OpenStackAdapter {
    pub(crate) client: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) username: String,
    pub(crate) password: String,
    pub(crate) project: String,
}

pub(crate) struct OpenStackAuth {
    pub(crate) token: String,
    pub(crate) compute_url: String,
}

impl OpenStackAdapter {
    pub fn new(connection: &ProviderConnection) -> UnifiedResult<Self> {
        let username = connection
            .credentials
            .username
            .clone()
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| UnifiedError::bad_request("OpenStack connection requires username"))?;
        let password = connection
            .credentials
            .password
            .clone()
            .filter(|password| !password.trim().is_empty())
            .ok_or_else(|| UnifiedError::bad_request("OpenStack connection requires password"))?;
        let project = connection.project_id.clone().unwrap_or_default();
        if project.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "OpenStack connection requires project_id (the Keystone project name)",
            ));
        }
        let endpoint = connection.endpoint.clone().unwrap_or_default();
        let url = Url::parse(endpoint.trim()).map_err(|_| {
            UnifiedError::bad_request("Invalid OpenStack endpoint URL")
        })?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(UnifiedError::bad_request(
                "OpenStack endpoint must use http or https",
            ));
        }
        Ok(Self {
            client: build_client()?,
            base_url: endpoint.trim().trim_end_matches('/').to_string(),
            username,
            password,
            project,
        })
    }

    pub(crate) async fn authenticate(&self) -> UnifiedResult<OpenStackAuth> {
        let body = serde_json::json!({
            "auth": {
                "identity": {
                    "methods": ["password"],
                    "password": {
                        "user": {
                            "name": self.username,
                            "domain": { "name": "Default" },
                            "password": self.password,
                        }
                    }
                },
                "scope": {
                    "project": {
                        "name": self.project,
                        "domain": { "name": "Default" },
                    }
                }
            }
        });
        let url = format!("{}/v3/auth/tokens", self.base_url);
        let response = self
            .client
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("OpenStack Keystone request failed: {error}"))
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(http_status_error(
                status,
                "openstack",
                parse_retry_after(&response),
            ));
        }
        let token = response
            .headers()
            .get("x-subject-token")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| {
                UnifiedError::unauthorized(
                    "OpenStack Keystone response has no token (HTTP 401 expected instead)",
                )
            })?;
        let body = response.json::<Value>().await.map_err(|error| {
            UnifiedError::internal(format!(
                "OpenStack Keystone returned an unreadable response: {error}"
            ))
        })?;
        let catalog = body
            .get("token")
            .and_then(|token| token.get("catalog"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut compute_url = None;
        for service in &catalog {
            if json_string(service, "type") != Some("compute") {
                continue;
            }
            if let Some(endpoints) = service.get("endpoints").and_then(Value::as_array) {
                for endpoint in endpoints {
                    if json_string(endpoint, "interface") == Some("public") {
                        compute_url = json_string(endpoint, "url").map(str::to_string);
                        break;
                    }
                }
            }
            if compute_url.is_some() {
                break;
            }
        }
        let compute_url = compute_url.ok_or_else(|| {
            UnifiedError::internal(
                "OpenStack service catalog has no public compute endpoint",
            )
        })?;
        Ok(OpenStackAuth { token, compute_url })
    }

    pub(crate) async fn compute_request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> UnifiedResult<Value> {
        let auth = self.authenticate().await?;
        let url = format!(
            "{}{path}",
            auth.compute_url.trim_end_matches('/')
        );
        let mut request = self.client.request(method, url);
        let token_value = HeaderValue::from_str(&auth.token).map_err(|_| {
            UnifiedError::internal("OpenStack token contains characters invalid in an HTTP header")
        })?;
        let mut headers = HeaderMap::new();
        headers.insert("X-Auth-Token", token_value);
        request = request.headers(headers);
        if let Some(body) = body {
            request = request.json(&body);
        }
        send_json(&self.client, request, "openstack").await
    }

    pub(crate) async fn compute_no_content(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> UnifiedResult<()> {
        let auth = self.authenticate().await?;
        let url = format!(
            "{}{path}",
            auth.compute_url.trim_end_matches('/')
        );
        let mut request = self.client.request(method, url);
        let token_value = HeaderValue::from_str(&auth.token).map_err(|_| {
            UnifiedError::internal("OpenStack token contains characters invalid in an HTTP header")
        })?;
        let mut headers = HeaderMap::new();
        headers.insert("X-Auth-Token", token_value);
        request = request.headers(headers);
        if let Some(body) = body {
            request = request.json(&body);
        }
        send_no_content(&self.client, request, "openstack").await
    }

    pub(crate) fn map_server(value: &Value) -> ServerInfo {
        let raw_status = json_string(value, "status").unwrap_or("").to_string();
        let mut public_ip = None;
        let mut private_ip = None;
        if let Some(addresses) = value.get("addresses").and_then(Value::as_object) {
            for network_addresses in addresses.values() {
                if let Some(list) = network_addresses.as_array() {
                    for address in list {
                        if json_string(address, "version") != Some("4") {
                            continue;
                        }
                        let ip = json_string(address, "ip").map(str::to_string);
                        match json_string(address, "OS-EXT-IPS:type") {
                            Some("floating") if public_ip.is_none() => public_ip = ip,
                            Some("fixed") if private_ip.is_none() => private_ip = ip,
                            _ => {}
                        }
                    }
                }
            }
        }
        let image = match value.get("image") {
            Some(Value::Object(image)) => {
                image.get("id").and_then(Value::as_str).map(str::to_string)
            }
            Some(Value::String(id)) if !id.is_empty() => Some(id.clone()),
            _ => None,
        };
        ServerInfo {
            id: json_string(value, "id").unwrap_or("").to_string(),
            name: json_string(value, "name").unwrap_or("").to_string(),
            item_kind: "server".to_string(),
            status: normalize_status(&raw_status).to_string(),
            raw_status: Some(raw_status),
            region: None,
            public_ip,
            private_ip,
            cpu_cores: None,
            memory_mb: None,
            disk_gb: None,
            image,
            created_at: json_string(value, "created").map(str::to_string),
        }
    }
}

#[async_trait]
impl CloudProvider for OpenStackAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenStack
    }

    async fn test_connection(&self) -> UnifiedResult<ProviderInfo> {
        let started = Instant::now();
        let auth = self.authenticate().await?;
        let url = format!("{}/servers?limit=1", auth.compute_url.trim_end_matches('/'));
        let mut request = self.client.get(url);
        let token_value = HeaderValue::from_str(&auth.token).map_err(|_| {
            UnifiedError::internal("OpenStack token contains characters invalid in an HTTP header")
        })?;
        let mut headers = HeaderMap::new();
        headers.insert("X-Auth-Token", token_value);
        request = request.headers(headers);
        send_json(&self.client, request, "openstack").await?;
        Ok(ProviderInfo {
            kind: "openstack".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: Some(self.project.clone()),
            region: None,
            detail: "OpenStack Keystone authentication succeeded".to_string(),
        })
    }

    async fn list_servers(&self) -> UnifiedResult<Vec<ServerInfo>> {
        let body = self
            .compute_request(
                Method::GET,
                &format!("/servers/detail?limit={MAX_LIST_LIMIT}"),
                None,
            )
            .await?;
        let servers = body
            .get("servers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(servers.iter().map(Self::map_server).collect())
    }

    async fn server_status(&self, id: &str) -> UnifiedResult<ServerInfo> {
        if id.trim().is_empty() {
            return Err(UnifiedError::bad_request("OpenStack server id is required"));
        }
        let body = self
            .compute_request(Method::GET, &format!("/servers/{}", encode_path_segment(id)), None)
            .await?;
        body.get("server")
            .map(Self::map_server)
            .ok_or_else(|| {
                UnifiedError::not_found(format!("OpenStack server \"{id}\" not found"))
            })
    }

    async fn create_server(&self, request: &CreateServerRequest) -> UnifiedResult<ServerInfo> {
        if request.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Server name is required"));
        }
        let image_ref = request
            .image_id
            .clone()
            .or_else(|| request.image.clone())
            .ok_or_else(|| {
                UnifiedError::bad_request("OpenStack create requires image or image_id (the glance image id)")
            })?;
        let flavor_ref = request.server_type.clone().ok_or_else(|| {
            UnifiedError::bad_request("OpenStack create requires server_type (the flavor id)")
        })?;
        // `networks: [{uuid: auto}]` lets Neutron auto-allocate the
        // default network; an explicit network choice is out of scope
        // for the generic create request.
        let body = self
            .compute_request(
                Method::POST,
                "/servers",
                Some(serde_json::json!({
                    "server": {
                        "name": request.name,
                        "imageRef": image_ref,
                        "flavorRef": flavor_ref,
                        "networks": [{ "uuid": "auto" }],
                    }
                })),
            )
            .await?;
        let id = body
            .get("server")
            .and_then(|server| json_string(server, "id"))
            .ok_or_else(|| {
                UnifiedError::internal("OpenStack create response has no server id")
            })?
            .to_string();
        self.server_status(&id).await
    }

    async fn delete_server(&self, id: &str) -> UnifiedResult<()> {
        self.compute_no_content(
            Method::DELETE,
            &format!("/servers/{}", encode_path_segment(id)),
            None,
        )
        .await
    }

    async fn start_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        self.compute_no_content(
            Method::POST,
            &format!("/servers/{}/action", encode_path_segment(id)),
            Some(serde_json::json!({ "os-start": null })),
        )
        .await?;
        self.server_status(id).await
    }

    async fn stop_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        self.compute_no_content(
            Method::POST,
            &format!("/servers/{}/action", encode_path_segment(id)),
            Some(serde_json::json!({ "os-stop": null })),
        )
        .await?;
        self.server_status(id).await
    }
}

// ---------------------------------------------------------------------------
// Proxmox VE adapter.
// ---------------------------------------------------------------------------

/// Proxmox VE API against a user-supplied host endpoint. Nodes +
/// QEMU / LXC guests list / status / start / stop / delete.
///
/// Auth: POST `/api2/json/access/ticket` with username (including
/// the realm, for example `root@pam`) and password; the ticket goes
/// into the `PVEAuthCookie` cookie and the CSRF token into the
/// `CSRFPreventionToken` header for writes. A fresh ticket is
/// fetched for every operation: adapters are short-lived here
/// (built per request), so there is no ticket cache in this round.
///
/// Guest ids are composite: `node:qemu:vmid` or `node:lxc:vmid`.
/// Stop uses the graceful `shutdown` action; a guest that ignores
/// the ACPI / shutdown signal stays running and the API reports an
/// error. Delete requires a stopped guest. Create is out of scope:
/// a bootable guest needs a template / ISO and storage choice the
/// generic create request cannot express.
pub struct ProxmoxAdapter {
    pub(crate) client: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) username: String,
    pub(crate) password: String,
}

pub(crate) struct ProxmoxTicket {
    pub(crate) ticket: String,
    pub(crate) csrf_token: String,
}

impl ProxmoxAdapter {
    pub fn new(connection: &ProviderConnection) -> UnifiedResult<Self> {
        let username = connection
            .credentials
            .username
            .clone()
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| UnifiedError::bad_request("Proxmox connection requires username"))?;
        let password = connection
            .credentials
            .password
            .clone()
            .filter(|password| !password.trim().is_empty())
            .ok_or_else(|| UnifiedError::bad_request("Proxmox connection requires password"))?;
        let endpoint = connection.endpoint.clone().unwrap_or_default();
        let url = Url::parse(endpoint.trim())
            .map_err(|_| UnifiedError::bad_request("Invalid Proxmox endpoint URL"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(UnifiedError::bad_request(
                "Proxmox endpoint must use http or https",
            ));
        }
        Ok(Self {
            client: build_client()?,
            base_url: endpoint.trim().trim_end_matches('/').to_string(),
            username,
            password,
        })
    }

    pub(crate) async fn ticket(&self) -> UnifiedResult<ProxmoxTicket> {
        let url = format!("{}/api2/json/access/ticket", self.base_url);
        let body = send_json(
            &self.client,
            self.client
                .post(url)
                .form(&[
                    ("username", self.username.as_str()),
                    ("password", self.password.as_str()),
                ]),
            "proxmox",
        )
        .await?;
        let data = body.get("data").ok_or_else(|| {
            UnifiedError::unauthorized("Proxmox ticket response has no data (check the username, realm, and password)")
        })?;
        let ticket = json_string(data, "ticket")
            .ok_or_else(|| {
                UnifiedError::unauthorized("Proxmox ticket response has no ticket")
            })?
            .to_string();
        let csrf_token = json_string(data, "CSRFPreventionToken")
            .ok_or_else(|| {
                UnifiedError::unauthorized("Proxmox ticket response has no CSRF token")
            })?
            .to_string();
        Ok(ProxmoxTicket { ticket, csrf_token })
    }

    /// One authenticated call; returns the `data` member of the
    /// Proxmox envelope (`{"data": ...}`).
    pub(crate) async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> UnifiedResult<Value> {
        let ticket = self.ticket().await?;
        let url = format!("{}/api2/json{path}", self.base_url);
        let mut request = self.client.request(method.clone(), url);
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::COOKIE,
            HeaderValue::from_str(&format!("PVEAuthCookie={}", ticket.ticket)).map_err(|_| {
                UnifiedError::internal("Proxmox ticket contains characters invalid in an HTTP header")
            })?,
        );
        if method != Method::GET {
            headers.insert(
                "CSRFPreventionToken",
                HeaderValue::from_str(&ticket.csrf_token).map_err(|_| {
                    UnifiedError::internal("Proxmox CSRF token contains characters invalid in an HTTP header")
                })?,
            );
        }
        request = request.headers(headers);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let body = send_json(&self.client, request, "proxmox").await?;
        Ok(body.get("data").cloned().unwrap_or(Value::Null))
    }

    pub(crate) fn map_guest(node: &str, guest_kind: &str, value: &Value) -> Option<ServerInfo> {
        let vmid = value.get("vmid").map(|id| id.to_string())?;
        let raw_status = json_string(value, "status").unwrap_or("").to_string();
        let bytes_to_mb = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_u64)
                .map(|bytes| bytes / 1024 / 1024)
        };
        let bytes_to_gb = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_u64)
                .map(|bytes| bytes / 1024 / 1024 / 1024)
        };
        Some(ServerInfo {
            id: format!("{node}:{guest_kind}:{vmid}"),
            name: json_string(value, "name").unwrap_or("").to_string(),
            item_kind: if guest_kind == "qemu" {
                "server".to_string()
            } else {
                "container".to_string()
            },
            status: normalize_status(&raw_status).to_string(),
            raw_status: Some(raw_status),
            region: Some(node.to_string()),
            public_ip: None,
            private_ip: None,
            cpu_cores: value
                .get("maxcpu")
                .and_then(Value::as_u64)
                .map(|cores| cores as u32),
            memory_mb: bytes_to_mb("maxmem"),
            disk_gb: bytes_to_gb("maxdisk"),
            image: None,
            created_at: None,
        })
    }

    pub(crate) fn parse_guest_id(id: &str) -> UnifiedResult<(String, String, String)> {
        let mut parts = id.split(':');
        let node = parts.next().unwrap_or("");
        let kind = parts.next().unwrap_or("");
        let vmid = parts.next().unwrap_or("");
        if node.is_empty()
            || (kind != "qemu" && kind != "lxc")
            || vmid.parse::<u32>().is_err()
            || parts.next().is_some()
        {
            return Err(UnifiedError::bad_request(format!(
                "Invalid Proxmox guest id \"{id}\" (expected \"node:qemu:vmid\" or \"node:lxc:vmid\")"
            )));
        }
        Ok((node.to_string(), kind.to_string(), vmid.to_string()))
    }

    pub(crate) async fn node_guests(&self, node: &str, guest_kind: &str) -> UnifiedResult<Vec<ServerInfo>> {
        let path = format!("/nodes/{}/{guest_kind}", encode_path_segment(node));
        let data = self.request(Method::GET, &path, None).await?;
        let guests = data.as_array().cloned().unwrap_or_default();
        Ok(guests
            .iter()
            .filter_map(|guest| Self::map_guest(node, guest_kind, guest))
            .collect())
    }
}

#[async_trait]
impl CloudProvider for ProxmoxAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Proxmox
    }

    async fn test_connection(&self) -> UnifiedResult<ProviderInfo> {
        let started = Instant::now();
        let data = self.request(Method::GET, "/nodes", None).await?;
        let node_count = data.as_array().map(Vec::len).unwrap_or(0);
        Ok(ProviderInfo {
            kind: "proxmox".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: None,
            region: None,
            detail: format!("Proxmox VE API reachable ({node_count} nodes)"),
        })
    }

    async fn list_servers(&self) -> UnifiedResult<Vec<ServerInfo>> {
        let data = self.request(Method::GET, "/nodes", None).await?;
        let nodes = data.as_array().cloned().unwrap_or_default();
        let mut servers = Vec::new();
        for node in &nodes {
            let Some(node_name) = json_string(node, "node") else {
                continue;
            };
            // Offline nodes cannot list guests; skip them instead of
            // failing the whole inventory.
            if json_string(node, "status") != Some("online") {
                continue;
            }
            for guest_kind in ["qemu", "lxc"] {
                servers.extend(self.node_guests(node_name, guest_kind).await?);
            }
        }
        Ok(servers)
    }

    async fn server_status(&self, id: &str) -> UnifiedResult<ServerInfo> {
        let (node, kind, _) = Self::parse_guest_id(id)?;
        let guests = self.node_guests(&node, &kind).await?;
        guests
            .into_iter()
            .find(|server| server.id == id)
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Proxmox guest \"{id}\" not found"))
            })
    }

    async fn delete_server(&self, id: &str) -> UnifiedResult<()> {
        let (node, kind, vmid) = Self::parse_guest_id(id)?;
        let path = format!(
            "/nodes/{}/{kind}/{vmid}",
            encode_path_segment(&node),
        );
        self.request(Method::DELETE, &path, None).await?;
        Ok(())
    }

    async fn start_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        let (node, kind, vmid) = Self::parse_guest_id(id)?;
        let path = format!(
            "/nodes/{}/{kind}/{vmid}/status/start",
            encode_path_segment(&node),
        );
        self.request(Method::POST, &path, None).await?;
        self.server_status(id).await
    }

    async fn stop_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        let (node, kind, vmid) = Self::parse_guest_id(id)?;
        let path = format!(
            "/nodes/{}/{kind}/{vmid}/status/shutdown",
            encode_path_segment(&node),
        );
        self.request(Method::POST, &path, None).await?;
        self.server_status(id).await
    }
}

// ---------------------------------------------------------------------------
// KVM adapter (through Proxmox VE).
// ---------------------------------------------------------------------------

/// KVM guests, surfaced through a Proxmox VE host: the same ticket
/// auth and QEMU listing as the Proxmox adapter, filtered to QEMU
/// guests only. LXC containers are exposed through the Proxmox
/// adapter instead.
///
/// Honest scope: plain KVM / libvirt exposes no HTTP API, so there
/// is no direct libvirt driver in this round. A host that is not
/// managed by Proxmox VE cannot be reached by this adapter.
pub struct KvmAdapter {
    pub(crate) proxmox: ProxmoxAdapter,
}

impl KvmAdapter {
    pub fn new(connection: &ProviderConnection) -> UnifiedResult<Self> {
        Ok(Self {
            proxmox: ProxmoxAdapter::new(connection)?,
        })
    }

    pub(crate) fn require_qemu(id: &str) -> UnifiedResult<(String, String, String)> {
        let parsed = ProxmoxAdapter::parse_guest_id(id)?;
        if parsed.1 != "qemu" {
            return Err(UnifiedError::bad_request(format!(
                "KVM adapter only manages QEMU guests; LXC container \"{id}\" belongs to the Proxmox adapter"
            )));
        }
        Ok(parsed)
    }
}

#[async_trait]
impl CloudProvider for KvmAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Kvm
    }

    async fn test_connection(&self) -> UnifiedResult<ProviderInfo> {
        let started = Instant::now();
        let info = self.proxmox.test_connection().await?;
        Ok(ProviderInfo {
            kind: "kvm".to_string(),
            reachable: info.reachable,
            latency_ms: started.elapsed().as_millis() as u64,
            account: info.account,
            region: info.region,
            detail: "Proxmox VE API reachable; KVM (QEMU) guests listed through it".to_string(),
        })
    }

    async fn list_servers(&self) -> UnifiedResult<Vec<ServerInfo>> {
        let servers = self.proxmox.list_servers().await?;
        Ok(servers
            .into_iter()
            .filter(|server| server.item_kind == "server")
            .collect())
    }

    async fn server_status(&self, id: &str) -> UnifiedResult<ServerInfo> {
        Self::require_qemu(id)?;
        self.proxmox.server_status(id).await
    }

    async fn delete_server(&self, id: &str) -> UnifiedResult<()> {
        Self::require_qemu(id)?;
        self.proxmox.delete_server(id).await
    }

    async fn start_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        Self::require_qemu(id)?;
        self.proxmox.start_server(id).await
    }

    async fn stop_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        Self::require_qemu(id)?;
        self.proxmox.stop_server(id).await
    }
}

// ---------------------------------------------------------------------------
// XenServer / XCP-ng adapter (XenAPI XML-RPC).
// ---------------------------------------------------------------------------

/// Minimal XML-RPC value helpers for XenAPI responses. XenAPI
/// answers XML-RPC; this codebase has no XML dependency, so these
/// are deliberately small string scanners for the known response
/// shapes, not a general XML parser. They must not be used for
/// anything else.
pub(crate) fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(crate) fn xmlrpc_string_param(value: &str) -> String {
    format!(
        "<param><value><string>{}</string></value></param>",
        xml_escape(value)
    )
}

pub(crate) fn xmlrpc_bool_param(value: bool) -> String {
    format!(
        "<param><value><boolean>{}</boolean></value></param>",
        if value { 1 } else { 0 }
    )
}

/// First `<string>...</string>` in the response (the session
/// reference for `session.login_with_password`, the VM reference
/// for `VM.get_by_uuid`).
pub(crate) fn xmlrpc_first_string(text: &str) -> Option<String> {
    xml_tag_at(text, 0, "string")
}

/// All `<string>` values inside the `<array>` of a response (the
/// VM references for `VM.get_all`).
pub(crate) fn xmlrpc_array_strings(text: &str) -> Vec<String> {
    let Some(array_at) = text.find("<array>") else {
        return Vec::new();
    };
    let Some(array_end) = text[array_at..].find("</array>") else {
        return Vec::new();
    };
    let array = &text[array_at..array_at + array_end];
    let mut values = Vec::new();
    let mut from = 0;
    while let Some(open) = array[from..].find("<string>") {
        let start = from + open + "<string>".len();
        let Some(close) = array[start..].find("</string>") else {
            break;
        };
        values.push(array[start..start + close].to_string());
        from = start + close + "</string>".len();
    }
    values
}

/// Flat scalar members of a XenAPI struct response (`VM.get_record`):
/// one `<member><name>X</name><value><string|int|boolean|double>`.
/// Nested `<struct>` / `<array>` values (for example
/// `other_config`) are skipped; only the scalar fields this adapter
/// maps are returned.
pub(crate) fn xmlrpc_struct_scalars(text: &str) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    let mut from = 0;
    while let Some(member_at) = text[from..].find("<member>") {
        let member_start = from + member_at + "<member>".len();
        let Some(member_end) = text[member_start..].find("</member>") else {
            break;
        };
        let member = &text[member_start..member_start + member_end];
        from = member_start + member_end + "</member>".len();
        let Some(name) = xml_tag_at(member, 0, "name") else {
            continue;
        };
        let Some(value_at) = member.find("<value>") else {
            continue;
        };
        let value_text = &member[value_at..];
        if value_text.starts_with("<value><struct>") || value_text.starts_with("<value><array>") {
            continue;
        }
        for tag in ["string", "int", "i4", "boolean", "double"] {
            if let Some(scalar) = xml_tag_at(value_text, 0, tag) {
                fields.insert(name, scalar);
                break;
            }
        }
    }
    fields
}

/// The `faultString` of an XML-RPC `<fault>` response, if any.
pub(crate) fn xmlrpc_fault_message(text: &str) -> Option<String> {
    let marker = "<name>faultString</name>";
    let at = text.find(marker)?;
    let rest = &text[at + marker.len()..];
    let open = rest.find("<string>")? + "<string>".len();
    let close = rest[open..].find("</string>")? + open;
    Some(rest[open..close].to_string())
}

/// XenServer / XCP-ng VMs over the XenAPI XML-RPC endpoint
/// (default path `/` on the host URL). VMs list / status / start /
/// stop / delete.
///
/// Auth: `session.login_with_password` per operation; the session
/// is logged out afterwards (best effort). A fresh session per
/// operation is deliberate: adapters are short-lived here (built
/// per request), so there is no session cache in this round.
///
/// Honest scope: the XML-RPC client above is hand-rolled for the
/// flat XenAPI shapes used here, and record fetching is one call
/// per VM. VM creation is out of scope: it needs a template clone
/// plus provision plus VIF / VBD wiring. Stop uses
/// `VM.clean_shutdown` (graceful); a VM without guest tools may
/// ignore it and the API reports an error. Delete
/// (`VM.destroy`) requires a halted VM.
pub struct XenAdapter {
    pub(crate) client: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) username: String,
    pub(crate) password: String,
}

impl XenAdapter {
    pub fn new(connection: &ProviderConnection) -> UnifiedResult<Self> {
        let username = connection
            .credentials
            .username
            .clone()
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| UnifiedError::bad_request("Xen connection requires username"))?;
        let password = connection
            .credentials
            .password
            .clone()
            .filter(|password| !password.trim().is_empty())
            .ok_or_else(|| UnifiedError::bad_request("Xen connection requires password"))?;
        let endpoint = connection.endpoint.clone().unwrap_or_default();
        let url = Url::parse(endpoint.trim())
            .map_err(|_| UnifiedError::bad_request("Invalid Xen endpoint URL"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(UnifiedError::bad_request(
                "Xen endpoint must use http or https",
            ));
        }
        Ok(Self {
            client: build_client()?,
            base_url: endpoint.trim().trim_end_matches('/').to_string(),
            username,
            password,
        })
    }

    /// One XML-RPC call. A `<fault>` response becomes an error
    /// carrying the provider's `faultString`; unknown-uuid faults
    /// map to 404.
    pub(crate) async fn rpc(&self, method: &str, params_xml: &str) -> UnifiedResult<String> {
        let body = format!(
            "<?xml version=\"1.0\"?><methodCall><methodName>{method}</methodName><params>{params_xml}</params></methodCall>"
        );
        let response = self
            .client
            .post(format!("{}/", self.base_url))
            .header(CONTENT_TYPE, "text/xml")
            .body(body)
            .send()
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("XenAPI request failed: {error}"))
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(http_status_error(status, "xen", parse_retry_after(&response)));
        }
        let text = response.text().await.map_err(|error| {
            UnifiedError::internal(format!("XenAPI returned an unreadable response: {error}"))
        })?;
        if text.contains("<fault>") {
            let fault = xmlrpc_fault_message(&text)
                .unwrap_or_else(|| "unknown XenAPI fault".to_string());
            if fault.contains("UUID_INVALID") || fault.contains("HANDLE_INVALID") {
                return Err(UnifiedError::not_found(format!("XenAPI fault: {fault}")));
            }
            return Err(UnifiedError::bad_request(format!("XenAPI fault: {fault}")));
        }
        Ok(text)
    }

    pub(crate) async fn login(&self) -> UnifiedResult<String> {
        let params = format!(
            "{}{}{}{}",
            xmlrpc_string_param(&self.username),
            xmlrpc_string_param(&self.password),
            xmlrpc_string_param("1.0"),
            xmlrpc_string_param("unified"),
        );
        let text = self.rpc("session.login_with_password", &params).await?;
        xmlrpc_first_string(&text).ok_or_else(|| {
            UnifiedError::unauthorized("XenAPI login returned no session reference")
        })
    }

    pub(crate) async fn logout(&self, session: &str) -> UnifiedResult<()> {
        self.rpc("session.logout", &xmlrpc_string_param(session))
            .await?;
        Ok(())
    }

    pub(crate) fn session_param(session: &str) -> String {
        xmlrpc_string_param(session)
    }

    pub(crate) async fn vm_ref_for_uuid(&self, session: &str, uuid: &str) -> UnifiedResult<String> {
        let params = format!(
            "{}{}",
            Self::session_param(session),
            xmlrpc_string_param(uuid),
        );
        let text = self.rpc("VM.get_by_uuid", &params).await?;
        xmlrpc_first_string(&text).ok_or_else(|| {
            UnifiedError::not_found(format!("Xen VM \"{uuid}\" not found"))
        })
    }

    pub(crate) async fn vm_record(&self, session: &str, vm_ref: &str) -> UnifiedResult<HashMap<String, String>> {
        let params = format!(
            "{}{}",
            Self::session_param(session),
            xmlrpc_string_param(vm_ref),
        );
        let text = self.rpc("VM.get_record", &params).await?;
        Ok(xmlrpc_struct_scalars(&text))
    }

    pub(crate) fn map_record(record: &HashMap<String, String>) -> Option<ServerInfo> {
        // Templates and the control domain are not guest VMs.
        if record.get("is_a_template").map(String::as_str) == Some("true") {
            return None;
        }
        if record.get("is_control_domain").map(String::as_str) == Some("true") {
            return None;
        }
        let uuid = record.get("uuid")?;
        if uuid.trim().is_empty() {
            return None;
        }
        let raw_status = record
            .get("power_state")
            .cloned()
            .unwrap_or_default();
        let memory_mb = record
            .get("memory_static_max")
            .and_then(|value| value.parse::<u64>().ok())
            .map(|bytes| bytes / 1024 / 1024);
        let cpu_cores = record
            .get("VCPUs_max")
            .and_then(|value| value.parse::<u32>().ok());
        Some(ServerInfo {
            id: uuid.clone(),
            name: record.get("name_label").cloned().unwrap_or_default(),
            item_kind: "server".to_string(),
            status: normalize_status(&raw_status).to_string(),
            raw_status: Some(raw_status),
            region: None,
            public_ip: None,
            private_ip: None,
            cpu_cores,
            memory_mb,
            disk_gb: None,
            image: None,
            created_at: None,
        })
    }

    pub(crate) async fn list_with_session(&self, session: &str) -> UnifiedResult<Vec<ServerInfo>> {
        let text = self
            .rpc("VM.get_all", &Self::session_param(session))
            .await?;
        let refs = xmlrpc_array_strings(&text);
        let mut servers = Vec::new();
        for vm_ref in refs {
            let record = self.vm_record(session, &vm_ref).await?;
            if let Some(server) = Self::map_record(&record) {
                servers.push(server);
            }
        }
        Ok(servers)
    }

    pub(crate) async fn record_for_uuid(&self, session: &str, uuid: &str) -> UnifiedResult<ServerInfo> {
        let vm_ref = self.vm_ref_for_uuid(session, uuid).await?;
        let record = self.vm_record(session, &vm_ref).await?;
        Self::map_record(&record).ok_or_else(|| {
            UnifiedError::not_found(format!("Xen VM \"{uuid}\" not found"))
        })
    }

    /// Run `work` with a fresh session, logging out afterwards
    /// (best effort: a logout failure never hides the real result).
    pub(crate) async fn with_session<T, F, Fut>(&self, work: F) -> UnifiedResult<T>
    where
        F: FnOnce(String) -> Fut,
        Fut: std::future::Future<Output = UnifiedResult<T>>,
    {
        let session = self.login().await?;
        let result = work(session.clone()).await;
        let _ = self.logout(&session).await;
        result
    }
}

#[async_trait]
impl CloudProvider for XenAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Xen
    }

    async fn test_connection(&self) -> UnifiedResult<ProviderInfo> {
        let started = Instant::now();
        let count = self
            .with_session(|session| async move {
                let text = self.rpc("VM.get_all", &Self::session_param(&session)).await?;
                Ok(xmlrpc_array_strings(&text).len())
            })
            .await?;
        Ok(ProviderInfo {
            kind: "xen".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: None,
            region: None,
            detail: format!("XenAPI reachable ({count} VMs)"),
        })
    }

    async fn list_servers(&self) -> UnifiedResult<Vec<ServerInfo>> {
        self.with_session(|session| async move { self.list_with_session(&session).await })
            .await
    }

    async fn server_status(&self, id: &str) -> UnifiedResult<ServerInfo> {
        if id.trim().is_empty() {
            return Err(UnifiedError::bad_request("Xen VM id (uuid) is required"));
        }
        self.with_session(|session| async move {
            self.record_for_uuid(&session, id).await
        })
        .await
    }

    async fn delete_server(&self, id: &str) -> UnifiedResult<()> {
        self.with_session(|session| async move {
            let vm_ref = self.vm_ref_for_uuid(&session, id).await?;
            let params = format!(
                "{}{}",
                Self::session_param(&session),
                xmlrpc_string_param(&vm_ref),
            );
            self.rpc("VM.destroy", &params).await?;
            Ok(())
        })
        .await
    }

    async fn start_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        self.with_session(|session| async move {
            let vm_ref = self.vm_ref_for_uuid(&session, id).await?;
            // VM.start takes (session, vm, start_paused, force).
            let params = format!(
                "{}{}{}{}",
                Self::session_param(&session),
                xmlrpc_string_param(&vm_ref),
                xmlrpc_bool_param(false),
                xmlrpc_bool_param(false),
            );
            self.rpc("VM.start", &params).await?;
            self.record_for_uuid(&session, id).await
        })
        .await
    }

    async fn stop_server(&self, id: &str) -> UnifiedResult<ServerInfo> {
        self.with_session(|session| async move {
            let vm_ref = self.vm_ref_for_uuid(&session, id).await?;
            let params = format!(
                "{}{}",
                Self::session_param(&session),
                xmlrpc_string_param(&vm_ref),
            );
            self.rpc("VM.clean_shutdown", &params).await?;
            self.record_for_uuid(&session, id).await
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_escape_covers_all_five_chars() {
        assert_eq!(xml_escape("a<b>&\"'c"), "a&lt;b&gt;&amp;&quot;&apos;c");
        assert_eq!(xml_escape("plain"), "plain");
    }

    #[test]
    fn xmlrpc_string_param_escapes_value() {
        assert_eq!(
            xmlrpc_string_param("x&y"),
            "<param><value><string>x&amp;y</string></value></param>"
        );
    }

    #[test]
    fn xmlrpc_bool_param_uses_one_and_zero() {
        assert!(xmlrpc_bool_param(true).contains("<boolean>1</boolean>"));
        assert!(xmlrpc_bool_param(false).contains("<boolean>0</boolean>"));
    }

    #[test]
    fn xmlrpc_first_string_reads_string_values_only() {
        assert_eq!(
            xmlrpc_first_string("<string>hello</string>"),
            Some("hello".to_string())
        );
        assert_eq!(xmlrpc_first_string("<int>5</int>"), None);
    }

    #[test]
    fn xmlrpc_fault_message_extracts_fault_string() {
        let text = "<fault><value><struct><member><name>faultString</name>\
            <value><string>bad session</string></value></member></struct></value></fault>";
        assert_eq!(xmlrpc_fault_message(text), Some("bad session".to_string()));
        assert_eq!(xmlrpc_fault_message("<ok/>"), None);
    }

    #[test]
    fn xmlrpc_array_strings_collects_values() {
        let text = "<array><value><string>a</string></value>\
            <value><string>b</string></value></array>";
        assert_eq!(xmlrpc_array_strings(text), vec!["a".to_string(), "b".to_string()]);
        assert!(xmlrpc_array_strings("no array").is_empty());
    }
}
