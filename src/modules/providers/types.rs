// providers module -- Shared types, records, request bodies, and constants.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;
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

/// Capability required to read provider kinds, connections, servers, and inventory.
pub(crate) const PERM_PROVIDERS_VIEW: &str = "providers.view";

/// Capability required to register / delete connections, test and sync them,
/// and to create, scale, delete, start, or stop servers.
pub(crate) const PERM_PROVIDERS_MANAGE: &str = "providers.manage";

// ---------------------------------------------------------------------------
// Limits.
// ---------------------------------------------------------------------------

/// Default HTTP timeout for provider API calls.
pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Default connect timeout for provider API calls.
pub(crate) const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 10;

/// Longest `Retry-After` delay honoured from a provider.
pub(crate) const MAX_RETRY_AFTER: Duration = Duration::from_secs(120);

/// Default inventory cache TTL in seconds.
pub(crate) const DEFAULT_INVENTORY_TTL_SECS: u64 = 300;

/// Maximum servers returned by one live list call (page size guard).
pub(crate) const MAX_LIST_LIMIT: u32 = 200;

/// Maximum length of a connection name (characters).
pub(crate) const MAX_CONNECTION_NAME_LEN: usize = 200;

/// Maximum length of a region (characters).
pub(crate) const MAX_REGION_LEN: usize = 128;

/// Maximum length of a project id (characters).
pub(crate) const MAX_PROJECT_ID_LEN: usize = 128;

/// Maximum length of a user-supplied endpoint URL (characters).
pub(crate) const MAX_ENDPOINT_LEN: usize = 2048;

/// User-Agent sent to provider APIs.
pub(crate) const USER_AGENT: &str = concat!("unified/", env!("CARGO_PKG_VERSION"), " providers");

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
#[derive(Clone, Default, Deserialize)]
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

impl fmt::Debug for ProviderCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Credential values are never written into a Debug string:
        // only whether each field is set is shown, so a future
        // debug log cannot dump plaintext secrets.
        let masked = |value: &Option<String>| -> &'static str {
            match value {
                Some(value) if !value.is_empty() => "[redacted]",
                _ => "[unset]",
            }
        };
        f.debug_struct("ProviderCredentials")
            .field("api_token", &masked(&self.api_token))
            .field("application_key", &masked(&self.application_key))
            .field("application_secret", &masked(&self.application_secret))
            .field("consumer_key", &masked(&self.consumer_key))
            .field("access_key_id", &masked(&self.access_key_id))
            .field("secret_access_key", &masked(&self.secret_access_key))
            .field("password", &masked(&self.password))
            .field("username", &masked(&self.username))
            .field("access_token", &masked(&self.access_token))
            .field("bearer_token", &masked(&self.bearer_token))
            .finish()
    }
}

impl ProviderCredentials {
    /// Names of the credential fields that are set (non-empty), for the
    /// masked read view. Values are never exposed.
    pub(crate) fn configured_fields(&self) -> Vec<&'static str> {
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
/// integrator -- see the migration sketch in the coordinator report.
#[derive(Clone)]
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

impl fmt::Debug for ProviderConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The credentials field uses its own redacted Debug impl, so
        // no secret value can leak through this struct either.
        f.debug_struct("ProviderConnection")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("project_id", &self.project_id)
            .field("credentials", &self.credentials)
            .field("custom_config", &self.custom_config)
            .field("enabled", &self.enabled)
            .field("last_tested_at", &self.last_tested_at)
            .field("last_status", &self.last_status)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

impl ProviderConnection {
    pub(crate) fn view(&self) -> ProviderConnectionView {
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

pub(crate) enum CustomAuth {
    Bearer(String),
    Basic(String, String),
    ApiKey(String, String),
    None,
}

// ---------------------------------------------------------------------------
// Request types and views.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct CreateConnectionRequest {
    pub(crate) name: String,
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) endpoint: Option<String>,
    #[serde(default)]
    pub(crate) region: Option<String>,
    #[serde(default)]
    pub(crate) project_id: Option<String>,
    #[serde(default)]
    pub(crate) credentials: ProviderCredentials,
    /// Config-driven definition for a `custom` connection (required
    /// for that kind, ignored for every other kind).
    #[serde(default)]
    pub(crate) custom_config: Option<CustomProviderConfig>,
    #[serde(default = "default_true")]
    pub(crate) enabled: bool,
}

pub(crate) fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub(crate) struct ProviderKindView {
    pub(crate) kind: String,
    pub(crate) display_name: String,
    pub(crate) capabilities: ProviderCapabilities,
    /// Fixed API hosts for this provider (empty when the endpoint is
    /// user-supplied).
    pub(crate) allowed_hosts: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct InventoryView {
    pub(crate) connection_id: Uuid,
    pub(crate) kind: String,
    pub(crate) servers: Vec<ServerInfo>,
    pub(crate) cached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fetched_at: Option<DateTime<Utc>>,
    pub(crate) ttl_secs: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct SyncView {
    pub(crate) connection_id: Uuid,
    pub(crate) kind: String,
    pub(crate) server_count: usize,
    pub(crate) synced_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_as_str_and_from_str_round_trip() {
        for kind in ProviderKind::all() {
            assert_eq!(ProviderKind::from_str(kind.as_str()), Some(*kind));
        }
    }

    #[test]
    fn all_kinds_lists_seventeen() {
        assert_eq!(ProviderKind::all().len(), 17);
    }

    #[test]
    fn from_str_rejects_unknown() {
        assert_eq!(ProviderKind::from_str("nope"), None);
        assert_eq!(ProviderKind::from_str(""), None);
    }

    #[test]
    fn hetzner_supports_create_scale_delete() {
        let caps = capabilities_for(ProviderKind::Hetzner);
        assert!(caps.supports_create);
        assert!(caps.supports_scale);
        assert!(caps.supports_delete);
    }

    #[test]
    fn aws_is_read_only() {
        let caps = capabilities_for(ProviderKind::Aws);
        assert!(!caps.supports_create);
        assert!(!caps.supports_scale);
        assert!(!caps.supports_delete);
        assert!(!caps.supports_containers);
        assert!(!caps.supports_start_stop);
    }

    #[test]
    fn display_name_is_human_readable() {
        assert_eq!(ProviderKind::Hetzner.display_name(), "Hetzner Cloud");
        assert_eq!(ProviderKind::Aws.display_name(), "AWS EC2");
    }

    #[test]
    fn debug_redacts_credential_values() {
        let creds = ProviderCredentials {
            api_token: Some("secret-token".to_string()),
            password: Some("hunter2".to_string()),
            ..Default::default()
        };
        let shown = format!("{creds:?}");
        assert!(!shown.contains("secret-token"));
        assert!(!shown.contains("hunter2"));
        assert!(shown.contains("[redacted]"));
        assert!(shown.contains("[unset]"));
    }

    #[test]
    fn connection_debug_redacts_credentials_but_keeps_name() {
        let connection = ProviderConnection {
            id: Uuid::now_v7(),
            name: "prod".to_string(),
            kind: ProviderKind::Hetzner,
            endpoint: None,
            region: None,
            project_id: None,
            credentials: ProviderCredentials {
                api_token: Some("secret-token".to_string()),
                ..Default::default()
            },
            custom_config: None,
            enabled: true,
            last_tested_at: None,
            last_status: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let shown = format!("{connection:?}");
        assert!(!shown.contains("secret-token"));
        assert!(shown.contains("prod"));
    }
}
