// Cloud / VPS / container provider connectors.
//
// One hookable provider registry with a common adapter interface, so new
// providers plug in through the registry instead of scattered code.
// Every adapter is an honest, typed HTTP client: it validates its
// configuration, tests the connection against the real provider API,
// and maps provider responses onto the shared `ServerInfo` shape. No
// adapter pretends to be a full vendor SDK.
//
// Scope of this round (deliberately minimal, per provider):
//
//   - Hetzner Cloud: servers list / status / create / scale (change
//     type) / delete.
//   - OVH: Public Cloud instances list / status / create / resize /
//     delete (one project at a time; the project id is the connection's
//     `project_id`).
//   - Tencent Cloud CVM: instances list / status only.
//   - Huawei Cloud ECS: instances list / status only (IAM token flow
//     or AK/SK request signing).
//   - AWS EC2: instances list / status only (SigV4 signed query API).
//     Object storage is out of scope here; there is no storage module
//     in this tree to reuse or duplicate.
//   - Google Cloud Compute Engine: instances list / status only, with
//     an OAuth2 access token supplied through settings. Token refresh
//     is out of scope; an expired token fails the connection test with
//     a clear 401.
//   - Virtualizor: VPS list / status / create / start / stop / delete
//     against a panel URL supplied on the connection.
//   - Docker: remote Engine API containers list / inspect / start /
//     stop (no image management, no create in this round).
//   - Kubernetes: namespaces, pods and deployments list / status
//     (read-only).
//   - Redshift: clusters list / status only (read-only; no query
//     execution in this round).
//   - DigitalOcean: droplets list / status / create / resize /
//     delete / power actions.
//   - Linode: instances list / status / create / resize / delete /
//     boot / shutdown / reboot.
//   - OpenStack: servers list / status / create / delete / start /
//     stop via Keystone auth + Nova (resize needs a confirm step and
//     is out of scope here).
//   - Proxmox VE: nodes + QEMU / LXC guests list / status / start /
//     stop / delete (ticket + CSRF auth; creating a bootable guest
//     needs a template / ISO choice the generic create request
//     cannot express, so create is out of scope here).
//   - KVM: the KVM (QEMU) guests of a Proxmox VE host, through the
//     Proxmox API. Plain KVM / libvirt exposes no HTTP API, so a
//     direct libvirt driver is out of scope for this round.
//   - XenServer / XCP-ng: VMs list / status / start / stop /
//     delete over the XenAPI XML-RPC endpoint (hand-rolled minimal
//     XML-RPC client; VM creation needs template clone + provision
//     and is out of scope here).
//   - Custom: a config-driven generic adapter — base URL, auth
//     type, endpoint templates, and response field mapping supplied
//     on the connection, so a new provider plugs in without a code
//     change (template-based mapping only; exotic response shapes
//     still need a real adapter).
//
// Credentials live only on the connection record, are never logged,
// and are never returned by any API response: read views expose only
// which credential fields are configured. Provider API hosts are
// allow-listed per adapter; user-supplied panel endpoints (Virtualizor,
// Docker, Kubernetes) must be http(s) URLs with a host and no embedded
// credentials. A 429 from a provider is surfaced as a 429 with the
// provider's `Retry-After` delay in `error.metadata`.
//
// Inventory is cached in memory per connection (default TTL 300s) and
// refreshed by the sync-now endpoint. Events emitted on the hook bus:
// `provider.connected`, `provider.sync.completed`,
// `provider.sync.failed`, `provider.server.created`. The audit log can
// subscribe to these (add them to `AUDITED_EVENTS` in the audit module)
// to record connection add/delete and create/scale actions.
//
// Wiring to the service monitor happens through hooks only: this module
// subscribes to `app.reload` (emitted by the service monitor reload
// endpoint) and clears the inventory cache, since a reload may change
// provider configuration. It never calls the service monitor directly.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};
use url::Url;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Events.
// ---------------------------------------------------------------------------

/// Emitted after a connection test succeeds.
pub const PROVIDER_CONNECTED: &str = "provider.connected";
/// Emitted after a sync-now refresh completes.
pub const PROVIDER_SYNC_COMPLETED: &str = "provider.sync.completed";
/// Emitted when a sync-now refresh fails.
pub const PROVIDER_SYNC_FAILED: &str = "provider.sync.failed";
/// Emitted when a server / container create succeeds.
pub const PROVIDER_SERVER_CREATED: &str = "provider.server.created";
/// Emitted when a connection is added or deleted (audit trail).
pub const PROVIDER_CONNECTION_CREATED: &str = "provider.connection.created";
pub const PROVIDER_CONNECTION_DELETED: &str = "provider.connection.deleted";
/// Emitted when a server is scaled or deleted (audit trail).
pub const PROVIDER_SERVER_SCALED: &str = "provider.server.scaled";
pub const PROVIDER_SERVER_DELETED: &str = "provider.server.deleted";

// ---------------------------------------------------------------------------
// Limits.
// ---------------------------------------------------------------------------

/// Default HTTP timeout for provider API calls.
const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// Default connect timeout for provider API calls.
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 10;
/// Longest `Retry-After` delay honoured from a provider.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(120);
/// Default inventory cache TTL in seconds.
const DEFAULT_INVENTORY_TTL_SECS: u64 = 300;
/// Maximum servers returned by one live list call (page size guard).
const MAX_LIST_LIMIT: u32 = 200;
/// User-Agent sent to provider APIs.
const USER_AGENT: &str = concat!("unified/", env!("CARGO_PKG_VERSION"), " providers");

// ---------------------------------------------------------------------------
// Provider kinds and capabilities.
// ---------------------------------------------------------------------------

/// Every cloud / VPS / container provider this module can talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Hetzner,
    Ovh,
    Tencent,
    Huawei,
    Aws,
    Google,
    Virtualizor,
    Docker,
    Kubernetes,
    Redshift,
    DigitalOcean,
    Linode,
    OpenStack,
    Proxmox,
    Kvm,
    Xen,
    Custom,
}

impl ProviderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Hetzner => "hetzner",
            Self::Ovh => "ovh",
            Self::Tencent => "tencent",
            Self::Huawei => "huawei",
            Self::Aws => "aws",
            Self::Google => "google",
            Self::Virtualizor => "virtualizor",
            Self::Docker => "docker",
            Self::Kubernetes => "kubernetes",
            Self::Redshift => "redshift",
            Self::DigitalOcean => "digitalocean",
            Self::Linode => "linode",
            Self::OpenStack => "openstack",
            Self::Proxmox => "proxmox",
            Self::Kvm => "kvm",
            Self::Xen => "xen",
            Self::Custom => "custom",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "hetzner" => Some(Self::Hetzner),
            "ovh" => Some(Self::Ovh),
            "tencent" => Some(Self::Tencent),
            "huawei" => Some(Self::Huawei),
            "aws" => Some(Self::Aws),
            "google" => Some(Self::Google),
            "virtualizor" => Some(Self::Virtualizor),
            "docker" => Some(Self::Docker),
            "kubernetes" => Some(Self::Kubernetes),
            "redshift" => Some(Self::Redshift),
            "digitalocean" => Some(Self::DigitalOcean),
            "linode" => Some(Self::Linode),
            "openstack" => Some(Self::OpenStack),
            "proxmox" => Some(Self::Proxmox),
            "kvm" => Some(Self::Kvm),
            "xen" => Some(Self::Xen),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }

    /// All kinds, in a stable order for the catalog endpoint.
    pub fn all() -> &'static [ProviderKind] {
        &[
            Self::Hetzner,
            Self::Ovh,
            Self::Tencent,
            Self::Huawei,
            Self::Aws,
            Self::Google,
            Self::Virtualizor,
            Self::Docker,
            Self::Kubernetes,
            Self::Redshift,
            Self::DigitalOcean,
            Self::Linode,
            Self::OpenStack,
            Self::Proxmox,
            Self::Kvm,
            Self::Xen,
            Self::Custom,
        ]
    }

    /// Human-readable display name.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Hetzner => "Hetzner Cloud",
            Self::Ovh => "OVHcloud",
            Self::Tencent => "Tencent Cloud CVM",
            Self::Huawei => "Huawei Cloud ECS",
            Self::Aws => "AWS EC2",
            Self::Google => "Google Cloud Compute Engine",
            Self::Virtualizor => "Virtualizor",
            Self::Docker => "Docker Engine",
            Self::Kubernetes => "Kubernetes",
            Self::Redshift => "Amazon Redshift",
            Self::DigitalOcean => "DigitalOcean",
            Self::Linode => "Linode",
            Self::OpenStack => "OpenStack",
            Self::Proxmox => "Proxmox VE",
            Self::Kvm => "KVM (via Proxmox VE)",
            Self::Xen => "XenServer / XCP-ng",
            Self::Custom => "Custom Provider",
        }
    }

    /// Hosts this provider's API is allowed to talk to. A request to any
    /// other host is refused before it is sent.
    pub fn allowed_hosts(&self) -> &'static [&'static str] {
        match self {
            Self::Hetzner => &["api.hetzner.cloud"],
            Self::Ovh => &["api.ovh.com", "eu.api.ovh.com"],
            Self::Tencent => &["cvm.tencentcloudapi.com"],
            Self::Huawei => &[],
            // Huawei endpoints are per-region (`iam.<region>.myhuaweicloud.com`,
            // `ecs.<region>.myhuaweicloud.com`); the host is validated by
            // `HuaweiAdapter::validate_endpoint` instead.
            Self::Aws => &[],
            // AWS EC2 endpoints are per-region (`ec2.<region>.amazonaws.com`);
            // validated by `AwsAdapter::validate_endpoint`.
            Self::Google => &["compute.googleapis.com", "oauth2.googleapis.com"],
            Self::Virtualizor | Self::Docker | Self::Kubernetes => &[],
            // User-supplied panel / API endpoints; validated as http(s)
            // URLs with a host and no embedded credentials.
            Self::Redshift => &[],
            // Redshift endpoints are per-region
            // (`redshift.<region>.amazonaws.com`); validated by
            // `RedshiftAdapter::validate_endpoint`.
            Self::DigitalOcean => &["api.digitalocean.com"],
            Self::Linode => &["api.linode.com"],
            Self::OpenStack | Self::Proxmox | Self::Kvm | Self::Xen | Self::Custom => &[],
            // User-supplied API endpoints (Keystone, Proxmox VE,
            // XenAPI, or a custom API base URL); validated as http(s)
            // URLs with a host and no embedded credentials.
        }
    }

    /// Whether the API host is fixed (allow-listed) or user-supplied.
    pub fn has_fixed_hosts(&self) -> bool {
        !matches!(
            self,
            Self::Huawei
                | Self::Aws
                | Self::Virtualizor
                | Self::Docker
                | Self::Kubernetes
                | Self::Redshift
                | Self::OpenStack
                | Self::Proxmox
                | Self::Kvm
                | Self::Xen
                | Self::Custom
        )
    }
}

/// What an adapter can do. Callers must check these flags before
/// calling create / scale / delete / start / stop: an unsupported call
/// fails with a 400 naming the missing capability.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ProviderCapabilities {
    pub supports_create: bool,
    pub supports_scale: bool,
    pub supports_delete: bool,
    /// True when `list_servers` returns containers / pods rather than
    /// virtual servers.
    pub supports_containers: bool,
    pub supports_start_stop: bool,
}

impl ProviderCapabilities {
    const fn read_only() -> Self {
        Self {
            supports_create: false,
            supports_scale: false,
            supports_delete: false,
            supports_containers: false,
            supports_start_stop: false,
        }
    }
}

/// Result of a connection test.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderInfo {
    pub kind: String,
    pub reachable: bool,
    pub latency_ms: u64,
    /// Account / project / region the test resolved, when the provider
    /// reports one (never a credential).
    pub account: Option<String>,
    pub region: Option<String>,
    pub detail: String,
}

/// One server, container, pod, deployment, or cluster, in the shared
/// shape every adapter maps onto.
#[derive(Debug, Clone, Serialize)]
pub struct ServerInfo {
    pub id: String,
    pub name: String,
    /// `server`, `container`, `pod`, `deployment`, or `cluster`.
    pub item_kind: String,
    /// Normalized status: `running`, `stopped`, `starting`, `stopping`,
    /// `creating`, `error`, or `unknown`.
    pub status: String,
    /// Provider-native status string, when it differs from `status`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_cores: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_gb: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Requests.
// ---------------------------------------------------------------------------

/// Generic create request. Adapters use the fields that apply to them
/// and reject the request when a field they require is missing.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateServerRequest {
    pub name: String,
    #[serde(default)]
    pub server_type: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    /// Hetzner image / OVH image id / Virtualizor OS template id,
    /// depending on the adapter. Kept separate from `image` so the
    /// caller can pass the provider-native identifier explicitly.
    #[serde(default)]
    pub image_id: Option<String>,
    /// Virtualizor plan id.
    #[serde(default)]
    pub plan_id: Option<String>,
    /// Root password for providers that create with one (Virtualizor).
    #[serde(default)]
    pub root_password: Option<String>,
    #[serde(default)]
    pub ssh_key_ids: Option<Vec<String>>,
}

/// Generic scale request. Adapters use the fields that apply to them.
#[derive(Debug, Clone, Deserialize)]
pub struct ScaleServerRequest {
    /// New server type / flavor id (Hetzner `change_type`, OVH resize).
    #[serde(default)]
    pub server_type: Option<String>,
    #[serde(default)]
    pub cpu_cores: Option<u32>,
    #[serde(default)]
    pub memory_mb: Option<u64>,
    #[serde(default)]
    pub disk_gb: Option<u64>,
}

// ---------------------------------------------------------------------------
// Credentials and connections.
// ---------------------------------------------------------------------------

/// Credentials for one connection. Every field is optional at the type
/// level; `validate_connection` enforces the per-kind required set.
/// Values are write-only: they are never serialized into a view, never
/// logged, and never included in hook payloads.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ProviderCredentials {
    /// Hetzner API token; Virtualizor API key; Docker/Kubernetes
    /// bearer token is `bearer_token` instead.
    #[serde(default)]
    pub api_token: Option<String>,
    /// OVH application key.
    #[serde(default)]
    pub application_key: Option<String>,
    /// OVH application secret.
    #[serde(default)]
    pub application_secret: Option<String>,
    /// OVH consumer key.
    #[serde(default)]
    pub consumer_key: Option<String>,
    /// Tencent secret id / AWS access key id / Huawei access key.
    #[serde(default)]
    pub access_key_id: Option<String>,
    /// Tencent secret key / AWS secret access key / Huawei secret key.
    #[serde(default)]
    pub secret_access_key: Option<String>,
    /// Virtualizor API password; Huawei IAM password.
    #[serde(default)]
    pub password: Option<String>,
    /// Huawei IAM username.
    #[serde(default)]
    pub username: Option<String>,
    /// Google Cloud OAuth2 access token.
    #[serde(default)]
    pub access_token: Option<String>,
    /// Docker / Kubernetes bearer token (service-account token).
    #[serde(default)]
    pub bearer_token: Option<String>,
}

impl ProviderCredentials {
    /// Names of the credential fields that are set (non-empty), for the
    /// masked read view. Values are never exposed.
    fn configured_fields(&self) -> Vec<&'static str> {
        let mut fields = Vec::new();
        let present = |value: &Option<String>| {
            value.as_deref().map(str::is_empty).unwrap_or(true) == false
        };
        if present(&self.api_token) {
            fields.push("api_token");
        }
        if present(&self.application_key) {
            fields.push("application_key");
        }
        if present(&self.application_secret) {
            fields.push("application_secret");
        }
        if present(&self.consumer_key) {
            fields.push("consumer_key");
        }
        if present(&self.access_key_id) {
            fields.push("access_key_id");
        }
        if present(&self.secret_access_key) {
            fields.push("secret_access_key");
        }
        if present(&self.password) {
            fields.push("password");
        }
        if present(&self.username) {
            fields.push("username");
        }
        if present(&self.access_token) {
            fields.push("access_token");
        }
        if present(&self.bearer_token) {
            fields.push("bearer_token");
        }
        fields
    }
}

/// One stored provider connection. Kept in memory (same pattern as the
/// downloader job store); durable persistence is a follow-up for the
/// integrator — see the migration sketch in the coordinator report.
#[derive(Debug, Clone)]
pub struct ProviderConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: ProviderKind,
    /// Panel / API endpoint for Virtualizor, Docker, Kubernetes, and
    /// the OVH endpoint choice. Empty for fixed-host providers.
    pub endpoint: Option<String>,
    /// Region (AWS, Tencent, Huawei, Google, Hetzner location, OVH
    /// region). Required for the region-scoped providers.
    pub region: Option<String>,
    /// OVH cloud project id (service name), Google Cloud project id,
    /// Huawei Cloud project id (resolved from the IAM token when
    /// empty), Kubernetes namespace filter.
    pub project_id: Option<String>,
    pub credentials: ProviderCredentials,
    /// Config-driven definition for `ProviderKind::Custom` (`None`
    /// for every other kind). Contains no credentials.
    pub custom_config: Option<CustomProviderConfig>,
    pub enabled: bool,
    pub last_tested_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Masked read view of a connection: credentials are never returned.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderConnectionView {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub kind_display: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Names of credential fields that are configured (values hidden).
    pub configured_credentials: Vec<String>,
    /// The config-driven definition for a custom provider (never
    /// contains credentials).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_config: Option<CustomProviderConfig>,
    pub capabilities: ProviderCapabilities,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_tested_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ProviderConnection {
    fn view(&self) -> ProviderConnectionView {
        // A custom connection reports its own configured flags; every
        // other kind reports the per-kind table.
        let capabilities = match (self.kind, &self.custom_config) {
            (ProviderKind::Custom, Some(config)) => config.capabilities(),
            _ => capabilities_for(self.kind),
        };
        ProviderConnectionView {
            id: self.id,
            name: self.name.clone(),
            kind: self.kind.as_str().to_string(),
            kind_display: self.kind.display_name().to_string(),
            endpoint: self.endpoint.clone(),
            region: self.region.clone(),
            project_id: self.project_id.clone(),
            configured_credentials: self
                .credentials
                .configured_fields()
                .into_iter()
                .map(str::to_string)
                .collect(),
            custom_config: self.custom_config.clone(),
            capabilities,
            enabled: self.enabled,
            last_tested_at: self.last_tested_at,
            last_status: self.last_status.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// Capabilities per kind (single source of truth for the catalog, the
/// read views, and the create/scale guards).
pub fn capabilities_for(kind: ProviderKind) -> ProviderCapabilities {
    match kind {
        ProviderKind::Hetzner => ProviderCapabilities {
            supports_create: true,
            supports_scale: true,
            supports_delete: true,
            supports_containers: false,
            supports_start_stop: false,
        },
        ProviderKind::Ovh => ProviderCapabilities {
            supports_create: true,
            supports_scale: true,
            supports_delete: true,
            supports_containers: false,
            supports_start_stop: false,
        },
        ProviderKind::Tencent | ProviderKind::Huawei | ProviderKind::Aws => {
            ProviderCapabilities::read_only()
        }
        ProviderKind::Google => ProviderCapabilities::read_only(),
        ProviderKind::Virtualizor => ProviderCapabilities {
            supports_create: true,
            supports_scale: true,
            supports_delete: true,
            supports_containers: false,
            supports_start_stop: true,
        },
        ProviderKind::Docker => ProviderCapabilities {
            supports_create: false,
            supports_scale: false,
            supports_delete: true,
            supports_containers: true,
            supports_start_stop: true,
        },
        ProviderKind::Kubernetes => ProviderCapabilities {
            supports_create: false,
            supports_scale: false,
            supports_delete: false,
            supports_containers: true,
            supports_start_stop: false,
        },
        ProviderKind::Redshift => ProviderCapabilities::read_only(),
        ProviderKind::DigitalOcean => ProviderCapabilities {
            supports_create: true,
            supports_scale: true,
            supports_delete: true,
            supports_containers: false,
            supports_start_stop: true,
        },
        ProviderKind::Linode => ProviderCapabilities {
            supports_create: true,
            supports_scale: true,
            supports_delete: true,
            supports_containers: false,
            supports_start_stop: true,
        },
        ProviderKind::OpenStack => ProviderCapabilities {
            supports_create: true,
            supports_scale: false,
            supports_delete: true,
            supports_containers: false,
            supports_start_stop: true,
        },
        ProviderKind::Proxmox | ProviderKind::Kvm | ProviderKind::Xen => {
            ProviderCapabilities {
                supports_create: false,
                supports_scale: false,
                supports_delete: true,
                supports_containers: false,
                supports_start_stop: true,
            }
        }
        // The generic adapter's maximum envelope. A stored custom
        // config can narrow it; `ProviderConnection::view` reports
        // the per-connection flags instead.
        ProviderKind::Custom => ProviderCapabilities {
            supports_create: true,
            supports_scale: false,
            supports_delete: true,
            supports_containers: false,
            supports_start_stop: true,
        },
    }
}

// ---------------------------------------------------------------------------
// Connection validation.
// ---------------------------------------------------------------------------

/// Required credential / field set per kind. `endpoint` and `region`
/// requirements are checked here too, so a bad connection is rejected
/// at creation time instead of failing its first API call.
fn validate_connection(input: &CreateConnectionRequest) -> UnifiedResult<()> {
    let kind = ProviderKind::from_str(&input.kind).ok_or_else(|| {
        UnifiedError::bad_request(format!("Unknown provider kind \"{}\"", input.kind))
    })?;
    if input.name.trim().is_empty() {
        return Err(UnifiedError::bad_request("Connection name is required"));
    }
    let creds = &input.credentials;
    let has = |value: &Option<String>| {
        value.as_deref().map(|v| !v.trim().is_empty()).unwrap_or(false)
    };
    let missing = |field: &str| {
        UnifiedError::bad_request(format!(
            "Provider \"{}\" requires credential field \"{field}\"",
            kind.as_str()
        ))
    };
    match kind {
        ProviderKind::Hetzner => {
            if !has(&creds.api_token) {
                return Err(missing("api_token"));
            }
        }
        ProviderKind::Ovh => {
            if !has(&creds.application_key) {
                return Err(missing("application_key"));
            }
            if !has(&creds.application_secret) {
                return Err(missing("application_secret"));
            }
            if !has(&creds.consumer_key) {
                return Err(missing("consumer_key"));
            }
            if input.project_id.as_deref().map(str::is_empty).unwrap_or(true) {
                return Err(UnifiedError::bad_request(
                    "Provider \"ovh\" requires project_id (the cloud project service name)",
                ));
            }
        }
        ProviderKind::Tencent => {
            if !has(&creds.access_key_id) {
                return Err(missing("access_key_id"));
            }
            if !has(&creds.secret_access_key) {
                return Err(missing("secret_access_key"));
            }
            if input.region.as_deref().map(str::is_empty).unwrap_or(true) {
                return Err(UnifiedError::bad_request(
                    "Provider \"tencent\" requires region",
                ));
            }
        }
        ProviderKind::Huawei => {
            let by_keys = has(&creds.access_key_id) && has(&creds.secret_access_key);
            let by_password = has(&creds.username) && has(&creds.password);
            if !by_keys && !by_password {
                return Err(UnifiedError::bad_request(
                    "Provider \"huawei\" requires access_key_id + secret_access_key, or username + password",
                ));
            }
            if input.region.as_deref().map(str::is_empty).unwrap_or(true) {
                return Err(UnifiedError::bad_request(
                    "Provider \"huawei\" requires region",
                ));
            }
        }
        ProviderKind::Aws => {
            if !has(&creds.access_key_id) {
                return Err(missing("access_key_id"));
            }
            if !has(&creds.secret_access_key) {
                return Err(missing("secret_access_key"));
            }
            if input.region.as_deref().map(str::is_empty).unwrap_or(true) {
                return Err(UnifiedError::bad_request(
                    "Provider \"aws\" requires region",
                ));
            }
        }
        ProviderKind::Google => {
            if !has(&creds.access_token) {
                return Err(missing("access_token"));
            }
            if input.project_id.as_deref().map(str::is_empty).unwrap_or(true) {
                return Err(UnifiedError::bad_request(
                    "Provider \"google\" requires project_id (the Google Cloud project id)",
                ));
            }
            if input.region.as_deref().map(str::is_empty).unwrap_or(true) {
                return Err(UnifiedError::bad_request(
                    "Provider \"google\" requires region (the zone, for example \"us-central1-a\")",
                ));
            }
        }
        ProviderKind::Virtualizor => {
            if !has(&creds.api_token) {
                return Err(missing("api_token"));
            }
            if !has(&creds.password) {
                return Err(missing("password"));
            }
            validate_user_endpoint(input.endpoint.as_deref(), "virtualizor")?;
        }
        ProviderKind::Docker => {
            validate_user_endpoint(input.endpoint.as_deref(), "docker")?;
        }
        ProviderKind::Kubernetes => {
            if !has(&creds.bearer_token) {
                return Err(missing("bearer_token"));
            }
            validate_user_endpoint(input.endpoint.as_deref(), "kubernetes")?;
        }
        ProviderKind::Redshift => {
            if !has(&creds.access_key_id) {
                return Err(missing("access_key_id"));
            }
            if !has(&creds.secret_access_key) {
                return Err(missing("secret_access_key"));
            }
            if input.region.as_deref().map(str::is_empty).unwrap_or(true) {
                return Err(UnifiedError::bad_request(
                    "Provider \"redshift\" requires region",
                ));
            }
        }
        ProviderKind::DigitalOcean => {
            if !has(&creds.api_token) {
                return Err(missing("api_token"));
            }
        }
        ProviderKind::Linode => {
            if !has(&creds.api_token) {
                return Err(missing("api_token"));
            }
        }
        ProviderKind::OpenStack => {
            if !has(&creds.username) {
                return Err(missing("username"));
            }
            if !has(&creds.password) {
                return Err(missing("password"));
            }
            if input.project_id.as_deref().map(str::is_empty).unwrap_or(true) {
                return Err(UnifiedError::bad_request(
                    "Provider \"openstack\" requires project_id (the Keystone project name)",
                ));
            }
            validate_user_endpoint(input.endpoint.as_deref(), "openstack")?;
        }
        ProviderKind::Proxmox => {
            if !has(&creds.username) {
                return Err(missing("username"));
            }
            if !has(&creds.password) {
                return Err(missing("password"));
            }
            validate_user_endpoint(input.endpoint.as_deref(), "proxmox")?;
        }
        ProviderKind::Kvm => {
            if !has(&creds.username) {
                return Err(missing("username"));
            }
            if !has(&creds.password) {
                return Err(missing("password"));
            }
            validate_user_endpoint(input.endpoint.as_deref(), "kvm")?;
        }
        ProviderKind::Xen => {
            if !has(&creds.username) {
                return Err(missing("username"));
            }
            if !has(&creds.password) {
                return Err(missing("password"));
            }
            validate_user_endpoint(input.endpoint.as_deref(), "xen")?;
        }
        ProviderKind::Custom => {
            validate_user_endpoint(input.endpoint.as_deref(), "custom")?;
            let Some(config) = &input.custom_config else {
                return Err(UnifiedError::bad_request(
                    "Provider \"custom\" requires custom_config (auth type, endpoint templates, and field mapping)",
                ));
            };
            validate_custom_config(config)?;
        }
    }
    // A fixed-host provider with a custom endpoint must still point at
    // one of its allow-listed hosts.
    if let Some(endpoint) = input.endpoint.as_deref() {
        if kind.has_fixed_hosts() {
            validate_fixed_endpoint(endpoint, kind)?;
        }
    }
    Ok(())
}

/// Validate a user-supplied panel / API endpoint (Virtualizor, Docker,
/// Kubernetes): http(s), a host, no embedded user info. The host itself
/// is admin-configured, so private / loopback hosts are allowed here —
/// the operator chose the endpoint.
fn validate_user_endpoint(endpoint: Option<&str>, kind: &str) -> UnifiedResult<Url> {
    let Some(raw) = endpoint else {
        return Err(UnifiedError::bad_request(format!(
            "Provider \"{kind}\" requires endpoint (the panel / API server URL)"
        )));
    };
    let url = Url::parse(raw.trim()).map_err(|_| {
        UnifiedError::bad_request(format!("Invalid endpoint URL \"{raw}\""))
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UnifiedError::bad_request(
            "Endpoint URL must use http or https",
        ));
    }
    if url.host_str().is_none_or(|host| host.is_empty()) {
        return Err(UnifiedError::bad_request(
            "Endpoint URL must include a host",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "Endpoint URL must not embed credentials; use the credential fields",
        ));
    }
    Ok(url)
}

/// Validate an endpoint override for a fixed-host provider against its
/// host allow-list.
fn validate_fixed_endpoint(endpoint: &str, kind: ProviderKind) -> UnifiedResult<()> {
    let url = Url::parse(endpoint.trim()).map_err(|_| {
        UnifiedError::bad_request(format!("Invalid endpoint URL \"{endpoint}\""))
    })?;
    let host = url.host_str().unwrap_or("");
    if !kind.allowed_hosts().contains(&host) {
        return Err(UnifiedError::bad_request(format!(
            "Endpoint host \"{host}\" is not an allowed {} API host",
            kind.as_str()
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// In-memory stores (same pattern as the downloader job store).
// ---------------------------------------------------------------------------

static CONNECTIONS: OnceLock<DashMap<Uuid, ProviderConnection>> = OnceLock::new();

fn connections_store() -> &'static DashMap<Uuid, ProviderConnection> {
    CONNECTIONS.get_or_init(DashMap::new)
}

/// Cached inventory per connection, with a fetch timestamp.
#[derive(Debug, Clone)]
struct CachedInventory {
    servers: Vec<ServerInfo>,
    fetched_at: DateTime<Utc>,
    fetched_instant: Instant,
}

static INVENTORY: OnceLock<DashMap<Uuid, CachedInventory>> = OnceLock::new();

fn inventory_store() -> &'static DashMap<Uuid, CachedInventory> {
    INVENTORY.get_or_init(DashMap::new)
}

// ---------------------------------------------------------------------------
// Provider registry: the plug-in point for adapters.
// ---------------------------------------------------------------------------

/// Factory that builds an adapter for one stored connection.
pub type ProviderFactory = Arc<
    dyn Fn(&ProviderConnection) -> UnifiedResult<Arc<dyn CloudProvider>> + Send + Sync,
>;

pub struct ProviderRegistry {
    factories: RwLock<HashMap<ProviderKind, ProviderFactory>>,
}

impl ProviderRegistry {
    fn new() -> Self {
        Self {
            factories: RwLock::new(HashMap::new()),
        }
    }

    /// Register (or replace) the factory for a kind. This is how a new
    /// provider plugs in: call it once at startup, or from an extension,
    /// instead of editing this module.
    pub fn register_factory(&self, kind: ProviderKind, factory: ProviderFactory) {
        self.factories
            .write()
            .expect("provider registry lock poisoned")
            .insert(kind, factory);
    }

    pub fn build(&self, connection: &ProviderConnection) -> UnifiedResult<Arc<dyn CloudProvider>> {
        let factories = self
            .factories
            .read()
            .expect("provider registry lock poisoned");
        let factory = factories.get(&connection.kind).ok_or_else(|| {
            UnifiedError::internal(format!(
                "No provider adapter registered for kind \"{}\"",
                connection.kind.as_str()
            ))
        })?;
        factory(connection)
    }

    pub fn registered_kinds(&self) -> Vec<ProviderKind> {
        let factories = self
            .factories
            .read()
            .expect("provider registry lock poisoned");
        let mut kinds: Vec<ProviderKind> = factories.keys().copied().collect();
        kinds.sort_by_key(|kind| kind.as_str());
        kinds
    }
}

static REGISTRY: OnceLock<ProviderRegistry> = OnceLock::new();

/// The shared registry, with the built-in adapters registered on
/// first use.
pub fn provider_registry() -> &'static ProviderRegistry {
    REGISTRY.get_or_init(|| {
        let registry = ProviderRegistry::new();
        registry.register_factory(
            ProviderKind::Hetzner,
            Arc::new(|connection| {
                Ok(Arc::new(HetznerAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Ovh,
            Arc::new(|connection| {
                Ok(Arc::new(OvhAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Tencent,
            Arc::new(|connection| {
                Ok(Arc::new(TencentAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Huawei,
            Arc::new(|connection| {
                Ok(Arc::new(HuaweiAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Aws,
            Arc::new(|connection| {
                Ok(Arc::new(AwsAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Google,
            Arc::new(|connection| {
                Ok(Arc::new(GoogleAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Virtualizor,
            Arc::new(|connection| {
                Ok(Arc::new(VirtualizorAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Docker,
            Arc::new(|connection| {
                Ok(Arc::new(DockerAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Kubernetes,
            Arc::new(|connection| {
                Ok(Arc::new(KubernetesAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Redshift,
            Arc::new(|connection| {
                Ok(Arc::new(RedshiftAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::DigitalOcean,
            Arc::new(|connection| {
                Ok(Arc::new(DigitalOceanAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Linode,
            Arc::new(|connection| {
                Ok(Arc::new(LinodeAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::OpenStack,
            Arc::new(|connection| {
                Ok(Arc::new(OpenStackAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Proxmox,
            Arc::new(|connection| {
                Ok(Arc::new(ProxmoxAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Kvm,
            Arc::new(|connection| {
                Ok(Arc::new(KvmAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Xen,
            Arc::new(|connection| {
                Ok(Arc::new(XenAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Custom,
            Arc::new(|connection| {
                Ok(Arc::new(CustomAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry
    })
}

// ---------------------------------------------------------------------------
// Common adapter interface.
// ---------------------------------------------------------------------------

/// The interface every provider adapter implements. Create / scale /
/// delete / start / stop have default implementations that fail with a
/// clear "not supported" error, so a read-only adapter only implements
/// the three required methods plus `capabilities`.
#[async_trait]
pub trait CloudProvider: Send + Sync {
    fn kind(&self) -> ProviderKind;

    fn capabilities(&self) -> ProviderCapabilities {
        capabilities_for(self.kind())
    }

    /// Verify the credentials against the real provider API.
    async fn test_connection(&self) -> UnifiedResult<ProviderInfo>;

    /// List servers / containers / clusters visible to the credentials.
    async fn list_servers(&self) -> UnifiedResult<Vec<ServerInfo>>;

    /// Status of one server / container / cluster by provider id.
    async fn server_status(&self, id: &str) -> UnifiedResult<ServerInfo>;

    /// Create a server. Guarded by `supports_create`.
    async fn create_server(&self, _request: &CreateServerRequest) -> UnifiedResult<ServerInfo> {
        Err(unsupported(
            self.kind(),
            "create",
            "supports_create",
        ))
    }

    /// Scale a server (change type / flavor / resources). Guarded by
    /// `supports_scale`.
    async fn scale_server(&self, _id: &str, _request: &ScaleServerRequest) -> UnifiedResult<ServerInfo> {
        Err(unsupported(self.kind(), "scale", "supports_scale"))
    }

    /// Delete a server / container. Guarded by `supports_delete`.
    async fn delete_server(&self, _id: &str) -> UnifiedResult<()> {
        Err(unsupported(self.kind(), "delete", "supports_delete"))
    }

    /// Start a container / VPS. Guarded by `supports_start_stop`.
    async fn start_server(&self, _id: &str) -> UnifiedResult<ServerInfo> {
        Err(unsupported(
            self.kind(),
            "start",
            "supports_start_stop",
        ))
    }

    /// Stop a container / VPS. Guarded by `supports_start_stop`.
    async fn stop_server(&self, _id: &str) -> UnifiedResult<ServerInfo> {
        Err(unsupported(self.kind(), "stop", "supports_start_stop"))
    }
}

fn unsupported(kind: ProviderKind, action: &str, capability: &str) -> UnifiedError {
    UnifiedError::bad_request(format!(
        "Provider \"{}\" does not support {action} (capability {capability} is false)",
        kind.as_str()
    ))
}

// ---------------------------------------------------------------------------
// Shared HTTP helpers.
// ---------------------------------------------------------------------------

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn sha256_hex(data: &[u8]) -> String {
    hex_encode(&Sha256::digest(data))
}

type HmacSha256 = Hmac<Sha256>;

fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key length is always valid");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

fn build_client() -> UnifiedResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS))
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))
}

/// Refuse a URL whose host is not on the provider's allow-list. Called
/// before every request to a fixed-host provider.
fn check_allowlist(url: &Url, kind: ProviderKind) -> UnifiedResult<()> {
    if !kind.has_fixed_hosts() {
        return Ok(());
    }
    let host = url.host_str().unwrap_or("");
    if !kind.allowed_hosts().contains(&host) {
        return Err(UnifiedError::bad_request(format!(
            "Refusing request to non-allow-listed host \"{host}\" for provider \"{}\"",
            kind.as_str()
        )));
    }
    Ok(())
}

/// Parse a `Retry-After` header (seconds only; HTTP dates are ignored),
/// capped so a misbehaving provider cannot stall a caller indefinitely.
fn parse_retry_after(response: &reqwest::Response) -> Option<Duration> {
    let value = response.headers().get(RETRY_AFTER)?.to_str().ok()?;
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(|secs| Duration::from_secs(secs).min(MAX_RETRY_AFTER))
}

/// Map a provider HTTP response to an error. A 429 keeps its status and
/// carries the `Retry-After` delay in metadata; 401/403/404 map to the
/// matching `UnifiedError`; other failures stay generic so provider
/// response bodies (which may contain account data) are never leaked
/// into the client-visible message.
fn http_status_error(
    status: StatusCode,
    provider: &str,
    retry_after: Option<Duration>,
) -> UnifiedError {
    let message = format!("Provider \"{provider}\" request failed with HTTP {status}");
    match status {
        StatusCode::TOO_MANY_REQUESTS => {
            let mut error = UnifiedError::new(message, 429)
                .with_metadata("provider", Value::String(provider.to_string()));
            if let Some(delay) = retry_after {
                error = error.with_metadata(
                    "retry_after_secs",
                    Value::from(delay.as_secs()),
                );
            }
            error
        }
        StatusCode::UNAUTHORIZED => UnifiedError::unauthorized(format!(
            "Provider \"{provider}\" rejected the credentials (HTTP 401)"
        )),
        StatusCode::FORBIDDEN => UnifiedError::forbidden(format!(
            "Provider \"{provider}\" refused the request (HTTP 403): the credentials lack permission for this operation"
        )),
        StatusCode::NOT_FOUND => {
            UnifiedError::not_found(format!("Provider \"{provider}\" resource not found (HTTP 404)"))
        }
        _ if status.is_client_error() => {
            UnifiedError::bad_request(message).with_metadata("provider", Value::String(provider.to_string()))
        }
        _ => UnifiedError::internal(message).with_metadata("provider", Value::String(provider.to_string())),
    }
}

/// Send a request and return the parsed JSON body, or a mapped error.
/// 429 responses are returned as errors (with `retry_after_secs` in
/// metadata) instead of being retried here, so the caller decides.
async fn send_json(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    provider: &str,
) -> UnifiedResult<Value> {
    let response = request.send().await.map_err(|error| {
        UnifiedError::internal(format!("Provider \"{provider}\" request failed: {error}"))
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(http_status_error(status, provider, parse_retry_after(&response)));
    }
    response.json::<Value>().await.map_err(|error| {
        UnifiedError::internal(format!(
            "Provider \"{provider}\" returned an unreadable response: {error}"
        ))
    })
}

/// Send a request and return the raw text body (for the XML query APIs
/// used by AWS EC2 and Redshift), or a mapped error.
async fn send_text(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    provider: &str,
) -> UnifiedResult<String> {
    let response = request.send().await.map_err(|error| {
        UnifiedError::internal(format!("Provider \"{provider}\" request failed: {error}"))
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(http_status_error(status, provider, parse_retry_after(&response)));
    }
    response.text().await.map_err(|error| {
        UnifiedError::internal(format!(
            "Provider \"{provider}\" returned an unreadable response: {error}"
        ))
    })
}

/// Send a request and discard the body (for 202 / 204 answers such
/// as deletes and power actions), or a mapped error.
async fn send_no_content(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    provider: &str,
) -> UnifiedResult<()> {
    let response = request.send().await.map_err(|error| {
        UnifiedError::internal(format!("Provider \"{provider}\" request failed: {error}"))
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(http_status_error(status, provider, parse_retry_after(&response)));
    }
    Ok(())
}

fn bearer_header(token: &str) -> UnifiedResult<HeaderMap> {
    let value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
        UnifiedError::bad_request("Stored credential contains characters invalid in an HTTP header")
    })?;
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, value);
    Ok(headers)
}

/// Normalize a provider-native status string onto the shared set.
fn normalize_status(raw: &str) -> &'static str {
    match raw.to_ascii_lowercase().as_str() {
        "running" | "active" | "on" | "up" | "available" | "ok" => "running",
        "stopped" | "off" | "down" | "terminated" | "deleted" | "paused" | "exited" | "dead"
        | "shutoff" | "suspended" | "shelved_offloaded" | "halted" | "archive" => "stopped",
        "starting" | "pending" | "provisioning" | "creating" | "initializing" | "restarting"
        | "new" | "build" | "rebuilding" | "rebooting" | "migrating" => "starting",
        "stopping" | "shutting-down" | "deleting" => "stopping",
        "creating" => "creating",
        "error" | "failed" | "crashed" | "unhealthy" => "error",
        _ => "unknown",
    }
}

fn json_string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn json_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn json_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

/// Percent-encode one URL path segment (RFC 3986 unreserved set is
/// kept as-is; everything else is encoded). Provider ids are normally
/// plain alphanumerics, but ids are caller-influenced input and must
/// never be interpolated raw into a URL path.
fn encode_path_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(*byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

// ---------------------------------------------------------------------------
// Minimal XML extraction (AWS EC2 / Redshift query APIs).
//
// The EC2 and Redshift query APIs answer XML, and this codebase has no
// XML dependency. This is a deliberately minimal extractor for the
// known response shapes (`DescribeInstances`, `DescribeClusters`):
// it finds `<tag>value</tag>` text and splits repeated `<item>` blocks.
// It is not a general XML parser and must not be used for anything else.
// ---------------------------------------------------------------------------

/// Text inside the first `<tag>...</tag>` at or after `from`, if any.
fn xml_tag_at(text: &str, from: usize, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text[from..].find(&open)? + from + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].trim().to_string())
}

/// Split `text` into the `<tag>...</tag>` blocks it contains.
fn xml_blocks(text: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut blocks = Vec::new();
    let mut from = 0;
    while let Some(start) = text[from..].find(&open) {
        let content_start = from + start + open.len();
        let Some(end) = text[content_start..].find(&close) else {
            break;
        };
        blocks.push(text[content_start..content_start + end].to_string());
        from = content_start + end + close.len();
    }
    blocks
}

// ---------------------------------------------------------------------------
// AWS SigV4 signing (EC2, Redshift).
//
// Canonical request, credential scope, and the four-step HMAC key
// derivation, following the AWS Signature Version 4 scheme. Only the
// `host`, `x-amz-date`, and `content-type` headers are signed, and the
// payload hash is always the SHA-256 of the (possibly empty) body.
// ---------------------------------------------------------------------------

struct SigV4Params {
    access_key_id: String,
    secret_access_key: String,
    region: String,
    service: String,
}

/// Build the `Authorization` header value and the `X-Amz-Date` value
/// for one request. `headers_to_sign` are `name: value` pairs, already
/// lower-case sorted; the caller must send exactly those headers.
fn sigv4_authorization(
    params: &SigV4Params,
    method: &str,
    url: &Url,
    headers_to_sign: &[(&str, String)],
    payload: &[u8],
    now: DateTime<Utc>,
) -> (String, String) {
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();
    let mut signed: Vec<(&str, String)> = headers_to_sign.to_vec();
    signed.sort_by(|a, b| a.0.cmp(b.0));
    let signed_headers: Vec<&str> = signed.iter().map(|(name, _)| *name).collect();
    let canonical_headers: String = signed
        .iter()
        .map(|(name, value)| format!("{name}:{}\n", value.trim()))
        .collect();
    let canonical_query = url.query().unwrap_or("");
    let canonical_request = format!(
        "{method}\n{}\n{canonical_query}\n{canonical_headers}{}\n{}",
        url.path(),
        signed_headers.join(";"),
        sha256_hex(payload),
    );
    let scope = format!("{date_stamp}/{}/{}/aws4_request", params.region, params.service);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes()),
    );
    let k_date = hmac_sha256(format!("AWS4{}", params.secret_access_key).as_bytes(), date_stamp.as_bytes());
    let k_region = hmac_sha256(&k_date, params.region.as_bytes());
    let k_service = hmac_sha256(&k_region, params.service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hex_encode(&hmac_sha256(&k_signing, string_to_sign.as_bytes()));
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={}, Signature={signature}",
        params.access_key_id,
        signed_headers.join(";"),
    );
    (authorization, amz_date)
}

// ---------------------------------------------------------------------------
// Tencent Cloud TC3-HMAC-SHA256 signing (CVM).
//
// Same HMAC-chain shape as SigV4 with Tencent's own prefixes: the
// signing key starts from `"TC3" + secret key`, the scope terminator
// is `tc3_request`, and the algorithm name is `TC3-HMAC-SHA256`.
// ---------------------------------------------------------------------------

struct Tc3Params {
    secret_id: String,
    secret_key: String,
    region: String,
    service: String,
    action: String,
    version: String,
}

fn tc3_headers(
    params: &Tc3Params,
    host: &str,
    payload: &[u8],
    now: DateTime<Utc>,
) -> UnifiedResult<HeaderMap> {
    let timestamp = now.timestamp();
    let date = now.format("%Y%m%d").to_string();
    let canonical_headers = format!(
        "content-type:application/json\nhost:{host}\nx-tc-action:{}\n",
        params.action.to_ascii_lowercase()
    );
    let signed_headers = "content-type;host;x-tc-action";
    let canonical_request = format!(
        "POST\n/\n\n{canonical_headers}{signed_headers}\n{}",
        sha256_hex(payload),
    );
    let scope = format!("{date}/{}/tc3_request", params.service);
    let string_to_sign = format!(
        "TC3-HMAC-SHA256\n{timestamp}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes()),
    );
    let k_date = hmac_sha256(format!("TC3{}", params.secret_key).as_bytes(), date.as_bytes());
    let k_service = hmac_sha256(&k_date, params.service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"tc3_request");
    let signature = hex_encode(&hmac_sha256(&k_signing, string_to_sign.as_bytes()));
    let authorization = format!(
        "TC3-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        params.secret_id,
    );
    let mut headers = HeaderMap::new();
    let set = |headers: &mut HeaderMap, name: &str, value: &str| -> UnifiedResult<()> {
        let header_value = HeaderValue::from_str(value).map_err(|_| {
            UnifiedError::bad_request("Computed signature contains characters invalid in an HTTP header")
        })?;
        headers.insert(
            reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                UnifiedError::internal("Invalid header name while signing a Tencent request")
            })?,
            header_value,
        );
        Ok(())
    };
    set(&mut headers, "authorization", &authorization)?;
    set(&mut headers, "content-type", "application/json")?;
    set(&mut headers, "host", host)?;
    set(&mut headers, "x-tc-action", &params.action)?;
    set(&mut headers, "x-tc-timestamp", &timestamp.to_string())?;
    set(&mut headers, "x-tc-version", &params.version)?;
    set(&mut headers, "x-tc-region", &params.region)?;
    Ok(headers)
}

// ---------------------------------------------------------------------------
// Huawei Cloud SDK-HMAC-SHA256 signing (ECS).
//
// Same key derivation shape as SigV4 with the `"SDK"` prefix and the
// `sdk_request` scope terminator. The signed headers are `host` and
// `x-sdk-date`.
// ---------------------------------------------------------------------------

fn huawei_authorization(
    access_key: &str,
    secret_key: &str,
    region: &str,
    service: &str,
    method: &str,
    url: &Url,
    payload: &[u8],
    now: DateTime<Utc>,
) -> UnifiedResult<(String, String)> {
    let sdk_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();
    let host = url.host_str().unwrap_or("").to_string();
    let canonical_headers = format!("host:{host}\nx-sdk-date:{sdk_date}\n");
    let canonical_request = format!(
        "{method}\n{}\n{}\n{canonical_headers}\nhost;x-sdk-date\n{}",
        url.path(),
        url.query().unwrap_or(""),
        sha256_hex(payload),
    );
    let scope = format!("{date_stamp}/{region}/{service}/sdk_request");
    let string_to_sign = format!(
        "SDK-HMAC-SHA256\n{sdk_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes()),
    );
    let k_date = hmac_sha256(format!("SDK{secret_key}").as_bytes(), date_stamp.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"sdk_request");
    let signature = hex_encode(&hmac_sha256(&k_signing, string_to_sign.as_bytes()));
    let authorization = format!(
        "SDK-HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders=host;x-sdk-date, Signature={signature}"
    );
    Ok((authorization, sdk_date))
}

// ---------------------------------------------------------------------------
// OVH signature.
//
// `"$1$" + SHA1(app_secret + "+" + consumer_key + "+" + method + "+" +
// url + "+" + body + "+" + timestamp)`, sent as `X-Ovh-Signature`
// alongside `X-Ovh-Application`, `X-Ovh-Consumer`, and
// `X-Ovh-Timestamp`.
// ---------------------------------------------------------------------------

fn ovh_signature(
    application_secret: &str,
    consumer_key: &str,
    method: &str,
    url: &str,
    body: &str,
    timestamp: i64,
) -> String {
    let to_sign = format!("{application_secret}+{consumer_key}+{method}+{url}+{body}+{timestamp}");
    format!("$1${}", hex_encode(&sha1::Sha1::digest(to_sign.as_bytes())))
}

// ---------------------------------------------------------------------------
// Hetzner Cloud adapter.
// ---------------------------------------------------------------------------

/// Hetzner Cloud API (`api.hetzner.cloud`), token auth.
pub struct HetznerAdapter {
    client: reqwest::Client,
    token: String,
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

    async fn request(
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

    fn map_server(value: &Value) -> ServerInfo {
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
}

// ---------------------------------------------------------------------------
// OVH adapter.
// ---------------------------------------------------------------------------

/// OVH API (`api.ovh.com` / `eu.api.ovh.com`), application-key +
/// signature auth. One cloud project per connection.
pub struct OvhAdapter {
    client: reqwest::Client,
    base_url: String,
    application_key: String,
    application_secret: String,
    consumer_key: String,
    project_id: String,
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

    async fn request(
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

    fn map_instance(value: &Value) -> ServerInfo {
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
    client: reqwest::Client,
    secret_id: String,
    secret_key: String,
    region: String,
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

    async fn call(&self, action: &str, params: Value) -> UnifiedResult<Value> {
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

    fn map_instance(value: &Value) -> ServerInfo {
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
    client: reqwest::Client,
    region: String,
    username: Option<String>,
    password: Option<String>,
    access_key: Option<String>,
    secret_key: Option<String>,
    project_id: Option<String>,
    /// Cached IAM token: (token, project id, expiry).
    token_cache: RwLock<Option<(String, String, DateTime<Utc>)>>,
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

    fn ecs_base(&self) -> String {
        format!("https://ecs.{}.myhuaweicloud.com", self.region)
    }

    fn iam_base(&self) -> String {
        format!("https://iam.{}.myhuaweicloud.com", self.region)
    }

    /// Fetch (or reuse) an IAM token for the password mode.
    async fn iam_token(&self) -> UnifiedResult<(String, String)> {
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
        let url = Url::parse(&format!("{}/v3/auth/tokens", self.iam_base()))
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
    async fn auth_headers(
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

    async fn request(&self, method: Method, path: &str) -> UnifiedResult<Value> {
        // The project id comes from the IAM token in password mode, or
        // from the connection in AK/SK mode.
        let project_id = if self.username.is_some() && self.password.is_some() {
            self.iam_token().await?.1
        } else {
            self.project_id.clone().unwrap_or_default()
        };
        let url = Url::parse(&format!("{}/v2/{project_id}{path}", self.ecs_base()))
            .map_err(|_| UnifiedError::internal("Invalid Huawei ECS URL"))?;
        let headers = self.auth_headers(method.as_str(), &url, b"").await?;
        let request = self.client.request(method, url).headers(headers);
        send_json(&self.client, request, "huawei").await
    }

    fn map_server(value: &Value) -> ServerInfo {
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

// ---------------------------------------------------------------------------
// AWS EC2 adapter.
// ---------------------------------------------------------------------------

/// AWS EC2 (`ec2.<region>.amazonaws.com`), SigV4 signed query API.
/// DescribeInstances only in this round; the XML response is read with
/// the minimal extractor above.
pub struct AwsAdapter {
    client: reqwest::Client,
    params: SigV4Params,
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

    fn endpoint(&self) -> UnifiedResult<Url> {
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

    async fn describe_instances(&self, extra: &[(&str, &str)]) -> UnifiedResult<String> {
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

    fn map_instance(block: &str) -> ServerInfo {
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

    fn parse_instances(xml: &str) -> Vec<ServerInfo> {
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
/// token refresh is out of scope — an expired token fails the
/// connection test with a clear 401.
pub struct GoogleAdapter {
    client: reqwest::Client,
    access_token: String,
    project_id: String,
    zone: String,
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

    async fn request(&self, method: Method, path: &str) -> UnifiedResult<Value> {
        let url = Url::parse(&format!("https://compute.googleapis.com{path}"))
            .map_err(|_| UnifiedError::internal("Invalid Google Compute API URL"))?;
        check_allowlist(&url, ProviderKind::Google)?;
        let headers = bearer_header(&self.access_token)?;
        let request = self.client.request(method, url).headers(headers);
        send_json(&self.client, request, "google").await
    }

    fn instances_path(&self) -> String {
        format!(
            "/compute/v1/projects/{}/zones/{}/instances",
            self.project_id, self.zone
        )
    }

    fn map_instance(value: &Value) -> ServerInfo {
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
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    api_pass: String,
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
    async fn call(
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

    fn map_vs(value: &Value) -> ServerInfo {
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

    fn vs_list(body: &Value) -> Vec<Value> {
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
    client: reqwest::Client,
    base_url: String,
    bearer_token: Option<String>,
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

    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
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

    fn map_container(value: &Value) -> ServerInfo {
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

    fn map_inspect(value: &Value) -> ServerInfo {
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
    client: reqwest::Client,
    base_url: String,
    bearer_token: String,
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

    async fn get(&self, path: &str) -> UnifiedResult<Value> {
        let url = Url::parse(&format!("{}{path}", self.base_url))
            .map_err(|_| UnifiedError::internal("Invalid Kubernetes API URL"))?;
        let headers = bearer_header(&self.bearer_token)?;
        let request = self.client.get(url).headers(headers);
        send_json(&self.client, request, "kubernetes").await
    }

    fn meta<'a>(value: &'a Value) -> (&'a str, &'a str, Option<&'a str>) {
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

    fn map_pod(value: &Value) -> ServerInfo {
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

    fn map_deployment(value: &Value) -> ServerInfo {
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

// ---------------------------------------------------------------------------
// Redshift adapter.
// ---------------------------------------------------------------------------

/// Amazon Redshift (`redshift.<region>.amazonaws.com`), SigV4 signed
/// query API. Clusters list / status only — read-only scope, no query
/// execution in this round.
pub struct RedshiftAdapter {
    client: reqwest::Client,
    params: SigV4Params,
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

    async fn describe_clusters(&self, extra: &[(&str, &str)]) -> UnifiedResult<String> {
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

    fn map_cluster(block: &str) -> ServerInfo {
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

    fn parse_clusters(xml: &str) -> Vec<ServerInfo> {
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

// ---------------------------------------------------------------------------
// Custom (config-driven) provider.
// ---------------------------------------------------------------------------

/// Response field mapping for a custom provider: dot paths into the
/// provider's JSON item objects (`networks.v4.0.ip_address` walks
/// objects and arrays). Only `id`, `name`, and `status` are required;
/// every other field is optional.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomFieldMapping {
    pub id: String,
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub public_ip: Option<String>,
    #[serde(default)]
    pub private_ip: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
}

impl Default for CustomFieldMapping {
    fn default() -> Self {
        Self {
            id: "id".to_string(),
            name: "name".to_string(),
            status: "status".to_string(),
            region: None,
            public_ip: None,
            private_ip: None,
            image: None,
            created_at: None,
        }
    }
}

/// Provider-native action names for the generic power endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomPowerActions {
    pub start: String,
    pub stop: String,
}

impl Default for CustomPowerActions {
    fn default() -> Self {
        Self {
            start: "power_on".to_string(),
            stop: "power_off".to_string(),
        }
    }
}

/// Config-driven definition of a provider that has no dedicated
/// adapter. The connection's `endpoint` is the API base URL; this
/// config supplies the auth type, the endpoint templates, and the
/// response field mapping. Templates use `{id}` and `{action}`
/// placeholders; body templates use `{name}`, `{server_type}`,
/// `{image}`, `{region}`, and `{action}`.
///
/// Honest limits: this adapter only does template-based JSON
/// mapping. It cannot do request signing (SigV4, TC3, OAuth2
/// refresh), paginated list responses that need link following,
/// nested create flows (template clone + provision), or response
/// shapes that need more than dot-path field lookup. Those still
/// need a real adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomProviderConfig {
    /// `bearer` | `basic` | `api_key` | `none`.
    pub auth_type: String,
    /// Header name for `api_key` auth (default `X-API-Key`).
    #[serde(default)]
    pub api_key_header: Option<String>,
    /// Relative list path, for example `/servers`.
    pub list_path: String,
    /// Key holding the item array in the list response, when the
    /// response is an object (for example `droplets`). When empty,
    /// the list response itself must be a JSON array.
    #[serde(default)]
    pub list_response_key: Option<String>,
    /// Relative status path, containing `{id}`.
    pub status_path: String,
    /// Key holding the item object in the status / create response,
    /// when the response wraps it (for example `server`). When empty,
    /// the response itself is the item.
    #[serde(default)]
    pub item_response_key: Option<String>,
    /// Relative create path (required when `supports_create`).
    #[serde(default)]
    pub create_path: Option<String>,
    /// JSON body template for create, with `{name}`,
    /// `{server_type}`, `{image}`, and `{region}` placeholders.
    /// Default: `{"name":"{name}","size":"{server_type}",
    /// "image":"{image}","region":"{region}"}`.
    #[serde(default)]
    pub create_body_template: Option<String>,
    /// Relative delete path, containing `{id}` (required when
    /// `supports_delete`).
    #[serde(default)]
    pub delete_path: Option<String>,
    /// Relative power path, containing `{id}` and `{action}`
    /// (required when `supports_start_stop`).
    #[serde(default)]
    pub power_path: Option<String>,
    /// Provider-native action names for start / stop.
    #[serde(default)]
    pub power_actions: Option<CustomPowerActions>,
    /// JSON body template for power actions, with an `{action}`
    /// placeholder. Default: `{"type":"{action}"}`.
    #[serde(default)]
    pub power_body_template: Option<String>,
    pub fields: CustomFieldMapping,
    #[serde(default)]
    pub supports_create: bool,
    #[serde(default)]
    pub supports_delete: bool,
    #[serde(default)]
    pub supports_start_stop: bool,
}

impl Default for CustomProviderConfig {
    fn default() -> Self {
        Self {
            auth_type: "bearer".to_string(),
            api_key_header: None,
            list_path: String::new(),
            list_response_key: None,
            status_path: String::new(),
            item_response_key: None,
            create_path: None,
            create_body_template: None,
            delete_path: None,
            power_path: None,
            power_actions: None,
            power_body_template: None,
            fields: CustomFieldMapping::default(),
            supports_create: false,
            supports_delete: false,
            supports_start_stop: false,
        }
    }
}

impl CustomProviderConfig {
    pub fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            supports_create: self.supports_create,
            supports_scale: false,
            supports_delete: self.supports_delete,
            supports_containers: false,
            supports_start_stop: self.supports_start_stop,
        }
    }
}

/// Validate one endpoint template: relative, starting with `/`, no
/// scheme, no parent-directory segments.
fn validate_relative_path(path: &str, field: &str) -> UnifiedResult<()> {
    if !path.starts_with('/') {
        return Err(UnifiedError::bad_request(format!(
            "Custom provider {field} must be a relative path starting with \"/\""
        )));
    }
    if path.contains("://") || path.split('/').any(|segment| segment == "..") {
        return Err(UnifiedError::bad_request(format!(
            "Custom provider {field} must not contain a URL scheme or \"..\" segments"
        )));
    }
    Ok(())
}

/// Validate a custom provider config at connection creation time, so
/// a bad config is rejected before its first API call.
fn validate_custom_config(config: &CustomProviderConfig) -> UnifiedResult<()> {
    match config.auth_type.as_str() {
        "bearer" | "basic" | "api_key" | "none" => {}
        other => {
            return Err(UnifiedError::bad_request(format!(
                "Custom provider auth_type \"{other}\" is not supported (bearer, basic, api_key, or none)"
            )));
        }
    }
    if config.list_path.trim().is_empty() {
        return Err(UnifiedError::bad_request(
            "Custom provider requires list_path",
        ));
    }
    validate_relative_path(&config.list_path, "list_path")?;
    if config.status_path.trim().is_empty() {
        return Err(UnifiedError::bad_request(
            "Custom provider requires status_path",
        ));
    }
    validate_relative_path(&config.status_path, "status_path")?;
    if !config.status_path.contains("{id}") {
        return Err(UnifiedError::bad_request(
            "Custom provider status_path must contain an \"{id}\" placeholder",
        ));
    }
    if let Some(path) = &config.create_path {
        validate_relative_path(path, "create_path")?;
    }
    if let Some(path) = &config.delete_path {
        validate_relative_path(path, "delete_path")?;
        if !path.contains("{id}") {
            return Err(UnifiedError::bad_request(
                "Custom provider delete_path must contain an \"{id}\" placeholder",
            ));
        }
    }
    if let Some(path) = &config.power_path {
        validate_relative_path(path, "power_path")?;
        if !path.contains("{id}") || !path.contains("{action}") {
            return Err(UnifiedError::bad_request(
                "Custom provider power_path must contain \"{id}\" and \"{action}\" placeholders",
            ));
        }
    }
    if config.supports_create && config.create_path.is_none() {
        return Err(UnifiedError::bad_request(
            "Custom provider supports_create requires create_path",
        ));
    }
    if config.supports_delete && config.delete_path.is_none() {
        return Err(UnifiedError::bad_request(
            "Custom provider supports_delete requires delete_path",
        ));
    }
    if config.supports_start_stop {
        if config.power_path.is_none() {
            return Err(UnifiedError::bad_request(
                "Custom provider supports_start_stop requires power_path",
            ));
        }
        let actions = config.power_actions.as_ref();
        let missing = actions.is_none()
            || actions
                .map(|actions| actions.start.trim().is_empty() || actions.stop.trim().is_empty())
                .unwrap_or(true);
        if missing {
            return Err(UnifiedError::bad_request(
                "Custom provider supports_start_stop requires power_actions with non-empty start and stop names",
            ));
        }
    }
    for (template, field) in [
        (&config.create_body_template, "create_body_template"),
        (&config.power_body_template, "power_body_template"),
    ] {
        if let Some(template) = template {
            serde_json::from_str::<Value>(template).map_err(|_| {
                UnifiedError::bad_request(format!(
                    "Custom provider {field} is not valid JSON"
                ))
            })?;
        }
    }
    if config.fields.id.trim().is_empty()
        || config.fields.name.trim().is_empty()
        || config.fields.status.trim().is_empty()
    {
        return Err(UnifiedError::bad_request(
            "Custom provider fields requires non-empty id, name, and status paths",
        ));
    }
    Ok(())
}

/// Walk a dot path through JSON objects and arrays (`a.b.0.c`).
/// Array segments are numeric indexes; object segments are keys.
fn lookup_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for segment in path.split('.') {
        if segment.is_empty() {
            return None;
        }
        if current.is_array() {
            let index = segment.parse::<usize>().ok()?;
            current = current.as_array()?.get(index)?;
        } else {
            current = current.get(segment)?;
        }
    }
    Some(current)
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// JSON-escape one template value's content (without surrounding
/// quotes), so a value containing quotes cannot break the rendered
/// JSON body template.
fn json_escape_content(value: &str) -> String {
    let quoted = serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string());
    quoted
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or("")
        .to_string()
}

/// Render a JSON body template by replacing `{key}` placeholders.
fn render_body_template(template: &str, vars: &[(&str, &str)]) -> String {
    let mut rendered = template.to_string();
    for (key, value) in vars {
        rendered = rendered.replace(&format!("{{{key}}}"), &json_escape_content(value));
    }
    rendered
}

enum CustomAuth {
    Bearer(String),
    Basic(String, String),
    ApiKey(String, String),
    None,
}

/// Config-driven generic adapter: no provider-specific code, only the
/// config on the connection. See `CustomProviderConfig` for the
/// honest limits of template-based mapping.
pub struct CustomAdapter {
    client: reqwest::Client,
    base_url: String,
    auth: CustomAuth,
    config: CustomProviderConfig,
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

    fn request(
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

    fn optional_field(&self, value: &Value, path: &Option<String>) -> Option<String> {
        let path = path.as_deref()?;
        lookup_path(value, path).and_then(value_to_string)
    }

    fn map_item(&self, value: &Value) -> Option<ServerInfo> {
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

    fn unwrap_item<'a>(&self, body: &'a Value) -> Option<&'a Value> {
        match &self.config.item_response_key {
            Some(key) => body.get(key),
            None => Some(body),
        }
    }

    fn render_template_body(
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
    async fn power(&self, id: &str, start: bool) -> UnifiedResult<ServerInfo> {
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

// ---------------------------------------------------------------------------
// DigitalOcean adapter.
// ---------------------------------------------------------------------------

/// DigitalOcean API (`api.digitalocean.com`), bearer token auth.
/// Droplets list (paginated, up to 5 pages of 200) / status /
/// create / resize / delete / power actions.
pub struct DigitalOceanAdapter {
    client: reqwest::Client,
    token: String,
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

    fn builder(
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

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> UnifiedResult<Value> {
        let request = self.builder(method, path, body)?;
        send_json(&self.client, request, "digitalocean").await
    }

    fn map_droplet(value: &Value) -> ServerInfo {
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

// ---------------------------------------------------------------------------
// Linode adapter.
// ---------------------------------------------------------------------------

/// Linode API (`api.linode.com`), bearer token auth. Instances list
/// (paginated, up to 5 pages of 200) / status / create / resize /
/// delete / boot / shutdown.
pub struct LinodeAdapter {
    client: reqwest::Client,
    token: String,
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

    async fn request(
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

    fn map_instance(value: &Value) -> ServerInfo {
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
    client: reqwest::Client,
    base_url: String,
    username: String,
    password: String,
    project: String,
}

struct OpenStackAuth {
    token: String,
    compute_url: String,
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

    async fn authenticate(&self) -> UnifiedResult<OpenStackAuth> {
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

    async fn compute_request(
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

    async fn compute_no_content(
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

    fn map_server(value: &Value) -> ServerInfo {
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
            Some(Value::Object(image)) => json_string(image, "id").map(str::to_string),
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
    client: reqwest::Client,
    base_url: String,
    username: String,
    password: String,
}

struct ProxmoxTicket {
    ticket: String,
    csrf_token: String,
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

    async fn ticket(&self) -> UnifiedResult<ProxmoxTicket> {
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
    async fn request(
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

    fn map_guest(node: &str, guest_kind: &str, value: &Value) -> Option<ServerInfo> {
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

    fn parse_guest_id(id: &str) -> UnifiedResult<(String, String, String)> {
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

    async fn node_guests(&self, node: &str, guest_kind: &str) -> UnifiedResult<Vec<ServerInfo>> {
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
    proxmox: ProxmoxAdapter,
}

impl KvmAdapter {
    pub fn new(connection: &ProviderConnection) -> UnifiedResult<Self> {
        Ok(Self {
            proxmox: ProxmoxAdapter::new(connection)?,
        })
    }

    fn require_qemu(id: &str) -> UnifiedResult<(String, String, String)> {
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
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn xmlrpc_string_param(value: &str) -> String {
    format!(
        "<param><value><string>{}</string></value></param>",
        xml_escape(value)
    )
}

fn xmlrpc_bool_param(value: bool) -> String {
    format!(
        "<param><value><boolean>{}</boolean></value></param>",
        if value { 1 } else { 0 }
    )
}

/// First `<string>...</string>` in the response (the session
/// reference for `session.login_with_password`, the VM reference
/// for `VM.get_by_uuid`).
fn xmlrpc_first_string(text: &str) -> Option<String> {
    xml_tag_at(text, 0, "string")
}

/// All `<string>` values inside the `<array>` of a response (the
/// VM references for `VM.get_all`).
fn xmlrpc_array_strings(text: &str) -> Vec<String> {
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
fn xmlrpc_struct_scalars(text: &str) -> HashMap<String, String> {
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
fn xmlrpc_fault_message(text: &str) -> Option<String> {
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
    client: reqwest::Client,
    base_url: String,
    username: String,
    password: String,
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
    async fn rpc(&self, method: &str, params_xml: &str) -> UnifiedResult<String> {
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

    async fn login(&self) -> UnifiedResult<String> {
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

    async fn logout(&self, session: &str) -> UnifiedResult<()> {
        self.rpc("session.logout", &xmlrpc_string_param(session))
            .await?;
        Ok(())
    }

    fn session_param(session: &str) -> String {
        xmlrpc_string_param(session)
    }

    async fn vm_ref_for_uuid(&self, session: &str, uuid: &str) -> UnifiedResult<String> {
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

    async fn vm_record(&self, session: &str, vm_ref: &str) -> UnifiedResult<HashMap<String, String>> {
        let params = format!(
            "{}{}",
            Self::session_param(session),
            xmlrpc_string_param(vm_ref),
        );
        let text = self.rpc("VM.get_record", &params).await?;
        Ok(xmlrpc_struct_scalars(&text))
    }

    fn map_record(record: &HashMap<String, String>) -> Option<ServerInfo> {
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

    async fn list_with_session(&self, session: &str) -> UnifiedResult<Vec<ServerInfo>> {
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

    async fn record_for_uuid(&self, session: &str, uuid: &str) -> UnifiedResult<ServerInfo> {
        let vm_ref = self.vm_ref_for_uuid(session, uuid).await?;
        let record = self.vm_record(session, &vm_ref).await?;
        Self::map_record(&record).ok_or_else(|| {
            UnifiedError::not_found(format!("Xen VM \"{uuid}\" not found"))
        })
    }

    /// Run `work` with a fresh session, logging out afterwards
    /// (best effort: a logout failure never hides the real result).
    async fn with_session<T, F, Fut>(&self, work: F) -> UnifiedResult<T>
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

// ---------------------------------------------------------------------------
// Inventory cache and sync.
// ---------------------------------------------------------------------------

fn inventory_ttl() -> Duration {
    Duration::from_secs(DEFAULT_INVENTORY_TTL_SECS)
}

/// Read the cached inventory, or fetch live from the provider and
/// cache the result. `force_refresh` skips the cache.
async fn get_inventory(
    connection: &ProviderConnection,
    force_refresh: bool,
) -> UnifiedResult<(Vec<ServerInfo>, bool, Option<DateTime<Utc>>)> {
    if !force_refresh {
        if let Some(cached) = inventory_store().get(&connection.id) {
            if cached.fetched_instant.elapsed() < inventory_ttl() {
                return Ok((cached.servers.clone(), true, Some(cached.fetched_at)));
            }
        }
    }
    let adapter = provider_registry().build(connection)?;
    let servers = adapter.list_servers().await?;
    inventory_store().insert(
        connection.id,
        CachedInventory {
            servers: servers.clone(),
            fetched_at: Utc::now(),
            fetched_instant: Instant::now(),
        },
    );
    Ok((servers, false, Some(Utc::now())))
}

/// Refresh one connection's inventory and emit the sync events. Used
/// by the sync-now endpoint.
async fn sync_connection(ctx: &Arc<AppContext>, connection: &ProviderConnection) -> UnifiedResult<usize> {
    let result = get_inventory(connection, true).await;
    match result {
        Ok((servers, _, _)) => {
            let count = servers.len();
            if let Some(mut stored) = connections_store().get_mut(&connection.id) {
                stored.last_status = Some("synced".to_string());
                stored.updated_at = Utc::now();
            }
            ctx.hooks.emit(
                PROVIDER_SYNC_COMPLETED,
                serde_json::json!({
                    "actor": "api",
                    "connection_id": connection.id,
                    "kind": connection.kind.as_str(),
                    "resource": format!("provider-connection:{}", connection.id),
                    "server_count": count,
                }),
            );
            Ok(count)
        }
        Err(error) => {
            if let Some(mut stored) = connections_store().get_mut(&connection.id) {
                stored.last_status = Some(format!("sync failed: {}", error.message));
                stored.updated_at = Utc::now();
            }
            ctx.hooks.emit(
                PROVIDER_SYNC_FAILED,
                serde_json::json!({
                    "actor": "api",
                    "connection_id": connection.id,
                    "kind": connection.kind.as_str(),
                    "resource": format!("provider-connection:{}", connection.id),
                    "error": error.message,
                }),
            );
            Err(error)
        }
    }
}

// ---------------------------------------------------------------------------
// Request types and views.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateConnectionRequest {
    name: String,
    kind: String,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    credentials: ProviderCredentials,
    /// Config-driven definition for a `custom` connection (required
    /// for that kind, ignored for every other kind).
    #[serde(default)]
    custom_config: Option<CustomProviderConfig>,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize)]
struct ProviderKindView {
    kind: String,
    display_name: String,
    capabilities: ProviderCapabilities,
    /// Fixed API hosts for this provider (empty when the endpoint is
    /// user-supplied).
    allowed_hosts: Vec<String>,
}

#[derive(Debug, Serialize)]
struct InventoryView {
    connection_id: Uuid,
    kind: String,
    servers: Vec<ServerInfo>,
    cached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    fetched_at: Option<DateTime<Utc>>,
    ttl_secs: u64,
}

#[derive(Debug, Serialize)]
struct SyncView {
    connection_id: Uuid,
    kind: String,
    server_count: usize,
    synced_at: DateTime<Utc>,
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}

fn find_connection(id: Uuid) -> UnifiedResult<ProviderConnection> {
    connections_store()
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Provider connection \"{id}\" not found")))
}

fn require_enabled(connection: &ProviderConnection) -> UnifiedResult<()> {
    if connection.enabled {
        Ok(())
    } else {
        Err(UnifiedError::conflict(format!(
            "Provider connection \"{}\" is disabled",
            connection.name
        )))
    }
}

fn sort_servers(mut servers: Vec<ServerInfo>, params: &PaginationParams) -> Vec<ServerInfo> {
    let sort = params.allowed_sort(&["name", "status", "created_at"], "name");
    let descending = params.normalized().order == "desc";
    match sort.as_str() {
        "status" => servers.sort_by(|a, b| a.status.cmp(&b.status).then_with(|| a.name.cmp(&b.name))),
        "created_at" => servers.sort_by(|a, b| {
            a.created_at.cmp(&b.created_at).then_with(|| a.name.cmp(&b.name))
        }),
        _ => servers.sort_by(|a, b| a.name.cmp(&b.name)),
    }
    if descending {
        servers.reverse();
    }
    servers
}

fn filter_servers(mut servers: Vec<ServerInfo>, params: &PaginationParams) -> Vec<ServerInfo> {
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        servers.retain(|server| {
            server.name.to_lowercase().contains(&needle)
                || server.id.to_lowercase().contains(&needle)
                || server.status.to_lowercase().contains(&needle)
        });
    }
    if let Some(status) = params.filters.get("status") {
        servers.retain(|server| server.status == *status);
    }
    if let Some(item_kind) = params.filters.get("item_kind") {
        servers.retain(|server| server.item_kind == *item_kind);
    }
    servers
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

/// Catalog of provider kinds and their capabilities.
async fn list_kinds(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    let registered = provider_registry().registered_kinds();
    let kinds: Vec<ProviderKind> = ProviderKind::all()
        .iter()
        .copied()
        .filter(|kind| registered.contains(kind))
        .collect();
    let result: UnifiedResult<Vec<ProviderKindView>> = Ok(kinds
        .iter()
        .map(|kind| ProviderKindView {
            kind: kind.as_str().to_string(),
            display_name: kind.display_name().to_string(),
            capabilities: capabilities_for(*kind),
            allowed_hosts: kind
                .allowed_hosts()
                .iter()
                .map(|host| host.to_string())
                .collect(),
        })
        .collect());
    json::respond(result, debug)
}

async fn create_connection(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateConnectionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProviderConnectionView> = (|| {
        validate_connection(&body)?;
        let kind = ProviderKind::from_str(&body.kind).ok_or_else(|| {
            UnifiedError::bad_request(format!("Unknown provider kind \"{}\"", body.kind))
        })?;
        let now = Utc::now();
        let connection = ProviderConnection {
            id: Uuid::now_v7(),
            name: body.name.trim().to_string(),
            kind,
            endpoint: body
                .endpoint
                .as_ref()
                .map(|endpoint| endpoint.trim().to_string())
                .filter(|endpoint| !endpoint.is_empty()),
            region: body
                .region
                .as_ref()
                .map(|region| region.trim().to_string())
                .filter(|region| !region.is_empty()),
            project_id: body
                .project_id
                .as_ref()
                .map(|project| project.trim().to_string())
                .filter(|project| !project.is_empty()),
            credentials: body.credentials.clone(),
            custom_config: body.custom_config.clone(),
            enabled: body.enabled,
            last_tested_at: None,
            last_status: None,
            created_at: now,
            updated_at: now,
        };
        // Validate the adapter can be built from the stored values
        // before accepting the connection.
        provider_registry().build(&connection)?;
        let view = connection.view();
        connections_store().insert(connection.id, connection.clone());
        ctx.hooks.emit(
            PROVIDER_CONNECTION_CREATED,
            serde_json::json!({
                "actor": "api",
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "resource": format!("provider-connection:{}", connection.id),
                "name": connection.name,
            }),
        );
        Ok(view)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_connections(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<ProviderConnectionView> = connections_store()
            .iter()
            .map(|entry| entry.value().view())
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|connection| connection.name.to_lowercase().contains(&needle));
        }
        if let Some(kind) = params.filters.get("kind") {
            items.retain(|connection| connection.kind == *kind);
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|connection| {
                connection.last_status.as_deref() == Some(status.as_str())
            });
        }
        let sort = params.allowed_sort(&["name", "kind", "created_at"], "name");
        match sort.as_str() {
            "kind" => items.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name))),
            "created_at" => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
            _ => items.sort_by(|a, b| a.name.cmp(&b.name)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_connection(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProviderConnectionView> =
        find_connection(id.into_inner()).map(|connection| connection.view());
    json::respond(result, debug)
}

async fn delete_connection(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        let connection = find_connection(id)?;
        connections_store().remove(&id);
        inventory_store().remove(&id);
        ctx.hooks.emit(
            PROVIDER_CONNECTION_DELETED,
            serde_json::json!({
                "actor": "api",
                "connection_id": id,
                "kind": connection.kind.as_str(),
                "resource": format!("provider-connection:{id}"),
                "name": connection.name,
            }),
        );
        Ok(serde_json::json!({ "id": id, "deleted": true }))
    })();
    json::respond(result, debug)
}

async fn test_connection(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProviderInfo> = (|| async {
        let connection = find_connection(id.into_inner())?;
        require_enabled(&connection)?;
        let adapter = provider_registry().build(&connection)?;
        let info = adapter.test_connection().await?;
        if let Some(mut stored) = connections_store().get_mut(&connection.id) {
            stored.last_tested_at = Some(Utc::now());
            stored.last_status = Some("ok".to_string());
            stored.updated_at = Utc::now();
        }
        ctx.hooks.emit(
            PROVIDER_CONNECTED,
            serde_json::json!({
                "actor": "api",
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "resource": format!("provider-connection:{}", connection.id),
                "latency_ms": info.latency_ms,
            }),
        );
        Ok(info)
    })()
    .await;
    json::respond(result, debug)
}

async fn list_servers(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let connection = find_connection(path.into_inner())?;
        require_enabled(&connection)?;
        let force_refresh = params
            .filters
            .get("refresh")
            .map(|value| value == "true")
            .unwrap_or(false);
        let (servers, cached, fetched_at) = get_inventory(&connection, force_refresh).await?;
        let servers = sort_servers(filter_servers(servers, &params), &params);
        let mut page = paginate(servers, &params).to_json();
        if let Value::Object(map) = &mut page {
            map.insert("cached".to_string(), Value::Bool(cached));
            map.insert(
                "fetched_at".to_string(),
                fetched_at
                    .map(|time| Value::String(time.to_rfc3339()))
                    .unwrap_or(Value::Null),
            );
            map.insert(
                "connection_id".to_string(),
                Value::String(connection.id.to_string()),
            );
        }
        Ok(page)
    })()
    .await;
    json::respond(result, debug)
}

async fn get_inventory_view(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InventoryView> = (|| async {
        let connection = find_connection(path.into_inner())?;
        require_enabled(&connection)?;
        let (servers, cached, fetched_at) = get_inventory(&connection, false).await?;
        Ok(InventoryView {
            connection_id: connection.id,
            kind: connection.kind.as_str().to_string(),
            servers,
            cached,
            fetched_at,
            ttl_secs: DEFAULT_INVENTORY_TTL_SECS,
        })
    })()
    .await;
    json::respond(result, debug)
}

async fn get_server_status(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(Uuid, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ServerInfo> = (|| async {
        let (connection_id, server_id) = path.into_inner();
        let connection = find_connection(connection_id)?;
        require_enabled(&connection)?;
        let adapter = provider_registry().build(&connection)?;
        adapter.server_status(&server_id).await
    })()
    .await;
    json::respond(result, debug)
}

async fn create_server(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<Uuid>,
    body: web::Json<CreateServerRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ServerInfo> = (|| async {
        let connection = find_connection(path.into_inner())?;
        require_enabled(&connection)?;
        let adapter = provider_registry().build(&connection)?;
        if !adapter.capabilities().supports_create {
            return Err(unsupported(connection.kind, "create", "supports_create"));
        }
        let server = adapter.create_server(&body).await?;
        inventory_store().remove(&connection.id);
        ctx.hooks.emit(
            PROVIDER_SERVER_CREATED,
            serde_json::json!({
                "actor": "api",
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "resource": format!("provider-server:{}", server.id),
                "server_id": server.id,
                "server_name": server.name,
            }),
        );
        Ok(server)
    })()
    .await;
    match result {
        Ok(server) => json::success_response(server, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn scale_server(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(Uuid, String)>,
    body: web::Json<ScaleServerRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ServerInfo> = (|| async {
        let (connection_id, server_id) = path.into_inner();
        let connection = find_connection(connection_id)?;
        require_enabled(&connection)?;
        let adapter = provider_registry().build(&connection)?;
        if !adapter.capabilities().supports_scale {
            return Err(unsupported(connection.kind, "scale", "supports_scale"));
        }
        let server = adapter.scale_server(&server_id, &body).await?;
        inventory_store().remove(&connection.id);
        ctx.hooks.emit(
            PROVIDER_SERVER_SCALED,
            serde_json::json!({
                "actor": "api",
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "resource": format!("provider-server:{server_id}"),
                "server_id": server_id,
            }),
        );
        Ok(server)
    })()
    .await;
    json::respond(result, debug)
}

async fn delete_server(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(Uuid, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let (connection_id, server_id) = path.into_inner();
        let connection = find_connection(connection_id)?;
        require_enabled(&connection)?;
        let adapter = provider_registry().build(&connection)?;
        if !adapter.capabilities().supports_delete {
            return Err(unsupported(connection.kind, "delete", "supports_delete"));
        }
        adapter.delete_server(&server_id).await?;
        inventory_store().remove(&connection.id);
        ctx.hooks.emit(
            PROVIDER_SERVER_DELETED,
            serde_json::json!({
                "actor": "api",
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "resource": format!("provider-server:{server_id}"),
                "server_id": server_id,
            }),
        );
        Ok(serde_json::json!({ "id": server_id, "deleted": true }))
    })()
    .await;
    json::respond(result, debug)
}

async fn start_server(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(Uuid, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ServerInfo> = (|| async {
        let (connection_id, server_id) = path.into_inner();
        let connection = find_connection(connection_id)?;
        require_enabled(&connection)?;
        let adapter = provider_registry().build(&connection)?;
        if !adapter.capabilities().supports_start_stop {
            return Err(unsupported(
                connection.kind,
                "start",
                "supports_start_stop",
            ));
        }
        let server = adapter.start_server(&server_id).await?;
        inventory_store().remove(&connection.id);
        Ok(server)
    })()
    .await;
    json::respond(result, debug)
}

async fn stop_server(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(Uuid, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ServerInfo> = (|| async {
        let (connection_id, server_id) = path.into_inner();
        let connection = find_connection(connection_id)?;
        require_enabled(&connection)?;
        let adapter = provider_registry().build(&connection)?;
        if !adapter.capabilities().supports_start_stop {
            return Err(unsupported(
                connection.kind,
                "stop",
                "supports_start_stop",
            ));
        }
        let server = adapter.stop_server(&server_id).await?;
        inventory_store().remove(&connection.id);
        Ok(server)
    })()
    .await;
    json::respond(result, debug)
}

async fn sync_now(ctx: web::Data<Arc<AppContext>>, path: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SyncView> = (|| async {
        let connection = find_connection(path.into_inner())?;
        require_enabled(&connection)?;
        let count = sync_connection(&ctx, &connection).await?;
        Ok(SyncView {
            connection_id: connection.id,
            kind: connection.kind.as_str().to_string(),
            server_count: count,
            synced_at: Utc::now(),
        })
    })()
    .await;
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct ProvidersModule;

impl Module for ProvidersModule {
    fn id(&self) -> &'static str {
        "providers"
    }

    fn name(&self) -> &'static str {
        "Cloud Providers"
    }

    fn description(&self) -> &'static str {
        "Cloud, VPS, and container provider connectors: Hetzner, OVH, Tencent, Huawei, AWS EC2, Google Compute, Virtualizor, Docker, Kubernetes, Redshift, DigitalOcean, Linode, OpenStack, Proxmox VE, KVM (via Proxmox), XenServer / XCP-ng, and config-driven custom providers, with a hookable adapter registry, cached inventory, and sync."
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Wiring to the service monitor through hooks only: a config
        // reload (emitted by the service monitor reload endpoint)
        // invalidates the cached inventory, since provider
        // configuration may have changed.
        bus.register(
            events::APP_RELOAD,
            "providers",
            std::sync::Arc::new(|_context| {
                inventory_store().clear();
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/providers")
                .route("", web::get().to(list_kinds))
                .route("/connections", web::get().to(list_connections))
                .route("/connections", web::post().to(create_connection))
                .route("/connections/{id}", web::get().to(get_connection))
                .route("/connections/{id}", web::delete().to(delete_connection))
                .route("/connections/{id}/test", web::post().to(test_connection))
                .route("/connections/{id}/servers", web::get().to(list_servers))
                .route("/connections/{id}/servers", web::post().to(create_server))
                .route("/connections/{id}/servers/{server_id}", web::get().to(get_server_status))
                .route("/connections/{id}/servers/{server_id}", web::delete().to(delete_server))
                .route("/connections/{id}/servers/{server_id}/scale", web::post().to(scale_server))
                .route("/connections/{id}/servers/{server_id}/start", web::post().to(start_server))
                .route("/connections/{id}/servers/{server_id}/stop", web::post().to(stop_server))
                .route("/connections/{id}/inventory", web::get().to(get_inventory_view))
                .route("/connections/{id}/sync", web::post().to(sync_now)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/providers",
                owner: "providers",
                description: "List provider kinds and their capabilities.",
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/providers/connections",
                owner: "providers",
                description: "List provider connections (credentials masked).",
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/providers/connections",
                owner: "providers",
                description: "Register a provider connection (credentials write-only).",
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/providers/connections/{id}",
                owner: "providers",
                description: "Get one provider connection (credentials masked).",
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/providers/connections/{id}",
                owner: "providers",
                description: "Delete a provider connection and its cached inventory.",
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/providers/connections/{id}/test",
                owner: "providers",
                description: "Test a provider connection against the real provider API.",
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/providers/connections/{id}/servers",
                owner: "providers",
                description: "List servers / containers for a connection (paginated, cached).",
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/providers/connections/{id}/servers",
                owner: "providers",
                description: "Create a server where the provider supports it.",
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/providers/connections/{id}/servers/{server_id}",
                owner: "providers",
                description: "Status of one server / container.",
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/providers/connections/{id}/servers/{server_id}",
                owner: "providers",
                description: "Delete a server / container where the provider supports it.",
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/providers/connections/{id}/servers/{server_id}/scale",
                owner: "providers",
                description: "Scale a server where the provider supports it.",
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/providers/connections/{id}/servers/{server_id}/start",
                owner: "providers",
                description: "Start a container / VPS where the provider supports it.",
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/providers/connections/{id}/servers/{server_id}/stop",
                owner: "providers",
                description: "Stop a container / VPS where the provider supports it.",
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/providers/connections/{id}/inventory",
                owner: "providers",
                description: "Cached inventory for a connection with fetch metadata.",
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/providers/connections/{id}/sync",
                owner: "providers",
                description: "Refresh cached inventory now and emit sync events.",
            },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    // Force the built-in adapters to register before the module is used.
    let _ = provider_registry();
    registry.register(Arc::new(ProvidersModule));
}
