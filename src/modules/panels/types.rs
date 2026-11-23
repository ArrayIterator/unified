//! panels module - Panel kinds, capabilities, records, connections and validation.

use unified_core::error::{UnifiedError, UnifiedResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::net::{IpAddr, ToSocketAddrs};
use url::Url;
use uuid::Uuid;

use super::adapter::display_name;
use super::constants::COLLECTIONS;

/// URL path segment for a collection (`server_accounts` is exposed as
/// `server-accounts` in URLs).
pub fn collection_segment(collection: &str) -> &'static str {
    match collection {
        "server_accounts" => "server-accounts",
        "clients" => "clients",
        "products" => "products",
        "services" => "services",
        "domains" => "domains",
        "invoices" => "invoices",
        _ => "clients",
    }
}

/// Parse a URL segment (or a collection name) into a collection name.
pub fn collection_from_segment(segment: &str) -> Option<&'static str> {
    match segment {
        "clients" => Some("clients"),
        "products" => Some("products"),
        "services" => Some("services"),
        "domains" => Some("domains"),
        "invoices" => Some("invoices"),
        "server-accounts" | "server_accounts" => Some("server_accounts"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Panel kinds and capabilities.
// ---------------------------------------------------------------------------

/// Every hosting / billing panel this module can talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelKind {
    Whmcs,
    Cpanel,
    DirectAdmin,
    Plesk,
    CyberPanel,
    Generic,
}

impl PanelKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Whmcs => "whmcs",
            Self::Cpanel => "cpanel",
            Self::DirectAdmin => "directadmin",
            Self::Plesk => "plesk",
            Self::CyberPanel => "cyberpanel",
            Self::Generic => "generic",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "whmcs" => Some(Self::Whmcs),
            "cpanel" => Some(Self::Cpanel),
            "directadmin" => Some(Self::DirectAdmin),
            "plesk" => Some(Self::Plesk),
            "cyberpanel" => Some(Self::CyberPanel),
            "generic" => Some(Self::Generic),
            _ => None,
        }
    }

    /// All kinds, in a stable order for the catalog endpoint.
    pub fn all() -> &'static [PanelKind] {
        &[
            Self::Whmcs,
            Self::Cpanel,
            Self::DirectAdmin,
            Self::Plesk,
            Self::CyberPanel,
            Self::Generic,
        ]
    }

    /// Human-readable display name.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Whmcs => "WHMCS",
            Self::Cpanel => "cPanel / WHM",
            Self::DirectAdmin => "DirectAdmin",
            Self::Plesk => "Plesk",
            Self::CyberPanel => "CyberPanel",
            Self::Generic => "Generic panel",
        }
    }

    /// Credential field names the catalog reports as required for
    /// this kind (values are never exposed).
    pub fn required_credential_fields(&self) -> &'static [&'static str] {
        match self {
            Self::Whmcs => &["api_identifier", "api_secret"],
            Self::Cpanel => &["username", "api_token"],
            Self::DirectAdmin => &["username", "password_or_api_token"],
            Self::Plesk => &["api_key"],
            Self::CyberPanel => &["api_token"],
            Self::Generic => &["depends_on_auth_type"],
        }
    }
}

/// Which collections an adapter can fetch. Callers must check these
/// flags before calling a list method: an unsupported call fails with
/// a 400 naming the missing capability.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct PanelCapabilities {
    pub clients: bool,
    pub products: bool,
    pub services: bool,
    pub domains: bool,
    pub invoices: bool,
    pub server_accounts: bool,
}

impl PanelCapabilities {
    pub fn supports(&self, collection: &str) -> bool {
        match collection {
            "clients" => self.clients,
            "products" => self.products,
            "services" => self.services,
            "domains" => self.domains,
            "invoices" => self.invoices,
            "server_accounts" => self.server_accounts,
            _ => false,
        }
    }

    /// Collections this capability set supports, in stable order.
    pub fn supported_collections(&self) -> Vec<&'static str> {
        COLLECTIONS
            .iter()
            .copied()
            .filter(|collection| self.supports(collection))
            .collect()
    }
}

/// Capabilities per kind (single source of truth for the catalog, the
/// read views, and the sync guards). The generic adapter overrides
/// this at runtime from its configuration; see
/// `GenericAdapter::capabilities`.
pub fn capabilities_for(kind: PanelKind) -> PanelCapabilities {
    match kind {
        PanelKind::Whmcs => PanelCapabilities {
            clients: true,
            products: true,
            services: true,
            domains: true,
            invoices: true,
            server_accounts: false,
        },
        PanelKind::Cpanel => PanelCapabilities {
            clients: false,
            products: true,
            services: false,
            domains: true,
            invoices: false,
            server_accounts: true,
        },
        PanelKind::DirectAdmin => PanelCapabilities {
            clients: true,
            products: true,
            services: false,
            domains: true,
            invoices: false,
            server_accounts: true,
        },
        PanelKind::Plesk => PanelCapabilities {
            clients: true,
            products: true,
            services: true,
            domains: true,
            invoices: false,
            server_accounts: false,
        },
        PanelKind::CyberPanel => PanelCapabilities {
            clients: false,
            products: true,
            services: false,
            domains: true,
            invoices: false,
            server_accounts: true,
        },
        PanelKind::Generic => PanelCapabilities {
            clients: false,
            products: false,
            services: false,
            domains: false,
            invoices: false,
            server_accounts: false,
        },
    }
}

/// Result of a connection test.
#[derive(Debug, Clone, Serialize)]
pub struct PanelInfo {
    pub kind: String,
    pub reachable: bool,
    pub latency_ms: u64,
    /// Account / user the test resolved, when the panel reports one
    /// (never a credential).
    pub account: Option<String>,
    pub detail: String,
}

// ---------------------------------------------------------------------------
// Shared record shapes.
// ---------------------------------------------------------------------------

/// One billing client.
#[derive(Debug, Clone, Serialize)]
pub struct PanelClient {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

/// One product / package / plan.
#[derive(Debug, Clone, Serialize)]
pub struct PanelProduct {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub billing_cycle: Option<String>,
    pub status: String,
}

/// One service: a product owned by a client (a hosting account, a
/// subscription, a client product).
#[derive(Debug, Clone, Serialize)]
pub struct PanelService {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    pub status: String,
}

/// One domain.
#[derive(Debug, Clone, Serialize)]
pub struct PanelDomain {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// One invoice.
#[derive(Debug, Clone, Serialize)]
pub struct PanelInvoice {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
}

/// One server account: a hosting account on the panel server (a WHM
/// account, a DirectAdmin user, a CyberPanel website).
#[derive(Debug, Clone, Serialize)]
pub struct PanelServerAccount {
    pub id: String,
    pub username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
}

/// One live record fetched from a panel, before storage: the remote
/// id, a display name, a normalized status, and the full typed
/// payload (kept so the stored record stays inspectable).
#[derive(Debug, Clone)]
pub struct RemoteRecord {
    pub remote_id: String,
    pub name: String,
    pub status: String,
    pub payload: Value,
}

impl RemoteRecord {
    pub(crate) fn from_value<T: Serialize>(
        id: String,
        name: String,
        status: String,
        value: &T,
    ) -> Self {
        Self {
            remote_id: id,
            name,
            status,
            payload: serde_json::to_value(value).unwrap_or(Value::Null),
        }
    }
}

/// Aggregate result of an adapter-level `sync_all`: per-collection
/// counts of records the panel returned, plus per-collection errors
/// for collections that failed. This is the adapter view only; the
/// module-level sync (which stores records and resolves conflicts)
/// builds on `fetch_collection` and reports a richer
/// `PanelSyncResult`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PanelSyncCounts {
    pub counts: HashMap<String, usize>,
    pub errors: HashMap<String, String>,
    pub synced_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Credentials and connections.
// ---------------------------------------------------------------------------

/// Credentials for one connection. Every field is optional at the type
/// level; `validate_connection` enforces the per-kind required set.
/// Values are write-only: they are never serialized into a view, never
/// logged, and never included in hook payloads.
///
/// `Debug` is hand-written (not derived): a derived `Debug` would
/// print every secret value into any log line that formats the
/// connection. The manual impl only reports which fields are set.
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct PanelCredentials {
    /// WHM API token; DirectAdmin login key (alternative to password).
    #[serde(default)]
    pub api_token: Option<String>,
    /// Plesk API key; generic adapter API key.
    #[serde(default)]
    pub api_key: Option<String>,
    /// WHMCS API identifier; also the panel username for cPanel,
    /// DirectAdmin, and CyberPanel when username/password auth is used.
    #[serde(default)]
    pub api_identifier: Option<String>,
    /// WHMCS API secret.
    #[serde(default)]
    pub api_secret: Option<String>,
    /// Panel username (cPanel WHM user, DirectAdmin user, CyberPanel
    /// admin user, generic basic auth).
    #[serde(default)]
    pub username: Option<String>,
    /// Panel password (DirectAdmin user password, CyberPanel admin
    /// password, generic basic auth).
    #[serde(default)]
    pub password: Option<String>,
}

impl std::fmt::Debug for PanelCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render a credential value: only whether each field
        // is set. Anything that formats a connection (logs, error
        // paths, `{:?}` on a request struct) therefore cannot leak
        // a secret.
        let masked = |value: &Option<String>| value.as_deref().map(|_| "[redacted]");
        f.debug_struct("PanelCredentials")
            .field("api_token", &masked(&self.api_token))
            .field("api_key", &masked(&self.api_key))
            .field("api_identifier", &masked(&self.api_identifier))
            .field("api_secret", &masked(&self.api_secret))
            .field("username", &masked(&self.username))
            .field("password", &masked(&self.password))
            .finish()
    }
}

impl PanelCredentials {
    /// Names of the credential fields that are set (non-empty), for the
    /// masked read view. Values are never exposed.
    pub(crate) fn configured_fields(&self) -> Vec<&'static str> {
        let mut fields = Vec::new();
        let present =
            |value: &Option<String>| value.as_deref().map(str::is_empty).unwrap_or(true) == false;
        if present(&self.api_token) {
            fields.push("api_token");
        }
        if present(&self.api_key) {
            fields.push("api_key");
        }
        if present(&self.api_identifier) {
            fields.push("api_identifier");
        }
        if present(&self.api_secret) {
            fields.push("api_secret");
        }
        if present(&self.username) {
            fields.push("username");
        }
        if present(&self.password) {
            fields.push("password");
        }
        fields
    }

    /// Merge an update into these credentials: only fields present in
    /// the update replace the stored value.
    pub(crate) fn merge(&mut self, update: &PanelCredentials) {
        if update.api_token.is_some() {
            self.api_token.clone_from(&update.api_token);
        }
        if update.api_key.is_some() {
            self.api_key.clone_from(&update.api_key);
        }
        if update.api_identifier.is_some() {
            self.api_identifier.clone_from(&update.api_identifier);
        }
        if update.api_secret.is_some() {
            self.api_secret.clone_from(&update.api_secret);
        }
        if update.username.is_some() {
            self.username.clone_from(&update.username);
        }
        if update.password.is_some() {
            self.password.clone_from(&update.password);
        }
    }
}

/// Field mapping for one generic-adapter collection: which response
/// fields carry the id, the display name, and the status. Field names
/// may use dot paths (`user.id`). Defaults are `id`, `name`, and
/// `status`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GenericFieldMapping {
    #[serde(default)]
    pub id_field: Option<String>,
    #[serde(default)]
    pub name_field: Option<String>,
    #[serde(default)]
    pub status_field: Option<String>,
}

impl GenericFieldMapping {
    pub(crate) fn id_field(&self) -> &str {
        self.id_field.as_deref().unwrap_or("id")
    }
    pub(crate) fn name_field(&self) -> &str {
        self.name_field.as_deref().unwrap_or("name")
    }
    pub(crate) fn status_field(&self) -> &str {
        self.status_field.as_deref().unwrap_or("status")
    }
}

/// Configuration for the generic panel adapter.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GenericPanelConfig {
    /// Auth type: `none`, `bearer`, `basic`, `api_key_header`, or
    /// `api_key_query`.
    ///
    /// `api_key_query` is a legacy alias for header auth: the key is
    /// sent in the `api_key_header` header (default `X-API-Key`),
    /// never in the URL query string, because query strings land in
    /// access logs, error reports, and proxies. Panels that strictly
    /// require the key as a query parameter are not supported.
    #[serde(default)]
    pub auth_type: Option<String>,
    /// Header name for `api_key_header` auth (default `X-API-Key`).
    #[serde(default)]
    pub api_key_header: Option<String>,
    /// Query parameter name for `api_key_query` auth (default
    /// `api_key`). Retained for compatibility only: the key is sent
    /// in a header, never in the query string (see `auth_type`),
    /// so this value is ignored.
    #[serde(default)]
    pub api_key_param: Option<String>,
    /// Per-collection endpoint paths, relative to the connection
    /// endpoint (for example `"clients": "/api/clients"`). Only
    /// collections listed here are supported by the adapter.
    #[serde(default)]
    pub endpoints: HashMap<String, String>,
    /// JSON field holding the record array when the response is an
    /// object (for example `"data"` or `"items"`). When empty, an
    /// array response is used directly, and an object response falls
    /// back to its `data`, `items`, or collection-named field.
    #[serde(default)]
    pub list_field: Option<String>,
    /// Per-collection response field mapping.
    #[serde(default)]
    pub field_mapping: HashMap<String, GenericFieldMapping>,
}

impl GenericPanelConfig {
    pub(crate) fn auth_type(&self) -> &str {
        self.auth_type.as_deref().unwrap_or("none")
    }
}

/// One stored panel connection. Kept in memory (same pattern as the
/// provider connection store); durable persistence is a follow-up for
/// the integrator — see the migration sketch in the coordinator report.
#[derive(Debug, Clone)]
pub struct PanelConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: PanelKind,
    /// Panel base URL for every kind (WHMCS install URL, WHM host,
    /// DirectAdmin host, Plesk host, CyberPanel host, generic base URL).
    pub endpoint: Option<String>,
    pub credentials: PanelCredentials,
    /// Generic adapter configuration (kind `generic` only).
    pub generic_config: Option<GenericPanelConfig>,
    /// Explicit opt-in to a private / loopback endpoint for this
    /// connection. Default `false`: `validate_panel_endpoint`
    /// refuses private, loopback, and link-local hosts (and hosts
    /// that resolve to one) unless this is set, because an
    /// unauthenticated caller must never be able to point a panel
    /// connection at the internal network. Only set this for a
    /// panel that genuinely runs on the private network, and only
    /// after checking that the caller is authorized to configure it.
    pub allow_private_endpoint: bool,
    pub enabled: bool,
    pub last_tested_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Masked read view of a connection: credentials are never returned.
#[derive(Debug, Clone, Serialize)]
pub struct PanelConnectionView {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub kind_display: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Names of credential fields that are configured (values hidden).
    pub configured_credentials: Vec<String>,
    pub capabilities: PanelCapabilities,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generic_config: Option<GenericPanelConfig>,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_tested_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl PanelConnection {
    pub(crate) fn view(&self) -> PanelConnectionView {
        let capabilities = match self.kind {
            PanelKind::Generic => self
                .generic_config
                .as_ref()
                .map(generic_capabilities)
                .unwrap_or_else(|| capabilities_for(self.kind)),
            _ => capabilities_for(self.kind),
        };
        PanelConnectionView {
            id: self.id,
            name: self.name.clone(),
            kind: self.kind.as_str().to_string(),
            kind_display: self.kind.display_name().to_string(),
            endpoint: self.endpoint.clone(),
            configured_credentials: self
                .credentials
                .configured_fields()
                .into_iter()
                .map(str::to_string)
                .collect(),
            capabilities,
            generic_config: self.generic_config.clone(),
            enabled: self.enabled,
            last_tested_at: self.last_tested_at,
            last_status: self.last_status.clone(),
            last_synced_at: self.last_synced_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// Capabilities of a generic adapter, derived from its configured
/// endpoint map: a collection is supported exactly when an endpoint
/// path is configured for it.
pub fn generic_capabilities(config: &GenericPanelConfig) -> PanelCapabilities {
    let has = |collection: &str| config.endpoints.contains_key(collection);
    PanelCapabilities {
        clients: has("clients"),
        products: has("products"),
        services: has("services"),
        domains: has("domains"),
        invoices: has("invoices"),
        server_accounts: has("server_accounts"),
    }
}

// ---------------------------------------------------------------------------
// Connection validation.
// ---------------------------------------------------------------------------

/// True when the address is loopback (IPv4-mapped IPv6 loopback
/// included).
fn is_loopback_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(addr) => addr.is_loopback(),
        IpAddr::V6(addr) => addr.is_loopback() || addr.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback()),
    }
}

/// True when the address is private, link-local, or otherwise
/// reserved: never a valid panel target unless the connection
/// explicitly opts into a private endpoint.
fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(addr) => {
            addr.is_private()
                || addr.is_link_local()
                || addr.is_loopback()
                || addr.is_unspecified()
                || addr.is_broadcast()
                || addr.is_multicast()
                || addr.is_documentation()
                || unified_core::net::ipv4_is_benchmarking(&addr)
                || unified_core::net::ipv4_is_reserved(&addr)
        }
        IpAddr::V6(addr) => {
            // An IPv4-mapped address is judged as the IPv4 address
            // it maps to, so `::ffff:127.0.0.1` cannot smuggle a
            // loopback target past the check.
            if let Some(v4) = addr.to_ipv4_mapped() {
                return is_forbidden_ip(IpAddr::V4(v4));
            }
            addr.is_unspecified()
                || addr.is_multicast()
                || addr.is_loopback()
                || addr.is_unique_local()
                || (addr.segments()[0] & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (addr.segments()[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (addr.segments()[0] == 0x2001 && addr.segments()[1] == 0x0db8) // documentation 2001:db8::/32
                || (addr.segments()[0] == 0x2001
                    && addr.segments()[1] == 0x0002
                    && addr.segments()[2] == 0) // benchmarking 2001:2::/48
                || (addr.segments()[0] & 0xffc0) == 0xfec0 // site-local fec0::/10
                || !unified_core::net::ipv6_is_global(&addr) // any other reserved / non-global range
        }
    }
}

/// Resolve a URL host and require EVERY resolved address to be safe.
/// A mixed answer (one public, one private address) is refused: the
/// HTTP client picks among the resolved addresses, so any private
/// address in the answer could be the one fetched. A host that does
/// not resolve at all is refused as well (fail closed).
pub(crate) fn resolved_addresses_are_safe(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") || host.to_ascii_lowercase().ends_with(".localhost") {
        return false;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return !is_loopback_ip(ip) && !is_forbidden_ip(ip);
    }
    let port = url.port_or_known_default().unwrap_or(443);
    let Ok(addresses) = (host, port).to_socket_addrs() else {
        return false;
    };
    let mut any = false;
    for socket in addresses {
        any = true;
        let ip = socket.ip();
        if is_loopback_ip(ip) || is_forbidden_ip(ip) {
            return false;
        }
    }
    any
}

/// Validate a user-supplied panel endpoint: http(s), a host, no
/// embedded user info, and — unless `allow_private` is set — a host
/// that does not resolve to a private, loopback, link-local, or
/// reserved address. The old comment claimed private hosts were
/// fine because "the operator chose the endpoint"; that is no
/// defence once any authenticated caller (or any caller at all,
/// before auth was added) can create a connection, so the default
/// is now deny. `allow_private` is the explicit per-connection
/// opt-in for panels that genuinely run on the private network.
///
/// Note the remaining TOCTOU gap, shared with every DNS-based
/// check: the address can change between validation and fetch
/// (DNS rebinding). The redirect policy in `build_client` re-checks
/// every redirect target, and `check_panel_host` re-checks the
/// request host, which narrows but does not close that window.
pub(crate) fn validate_panel_endpoint(
    endpoint: Option<&str>,
    kind: &str,
    allow_private: bool,
) -> UnifiedResult<Url> {
    let Some(raw) = endpoint else {
        return Err(UnifiedError::bad_request(format!(
            "Panel \"{kind}\" requires endpoint (the panel base URL)"
        )));
    };
    let url = Url::parse(raw.trim())
        .map_err(|_| UnifiedError::bad_request(format!("Invalid endpoint URL \"{raw}\"")))?;
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
    if !allow_private && !resolved_addresses_are_safe(&url) {
        return Err(UnifiedError::bad_request(format!(
            "Panel \"{kind}\" endpoint host resolves to a private, loopback, link-local, or reserved address; refused. Set allow_private_endpoint only for a panel that genuinely runs on the private network"
        )));
    }
    Ok(url)
}

/// Validate one redirect target against the connection's base host:
/// same host, http(s), no embedded credentials, and (unless the
/// connection opted into a private endpoint) a host that does not
/// resolve to a forbidden address. Used by the redirect policy in
/// `build_client`, so a panel 302 to an internal host — or to any
/// other host — stops the chain instead of carrying the panel
/// credentials there.
pub(crate) fn redirect_target_allowed(
    url: &Url,
    allowed_host: &str,
    allow_private: bool,
) -> UnifiedResult<()> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UnifiedError::bad_request(
            "Redirect target must use http or https",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "Redirect target must not embed credentials",
        ));
    }
    let host = url.host_str().unwrap_or("");
    if !host.eq_ignore_ascii_case(allowed_host) {
        return Err(UnifiedError::bad_request(format!(
            "Refusing redirect to host \"{host}\": panel is only allowed to talk to its configured host \"{allowed_host}\""
        )));
    }
    if !allow_private && !resolved_addresses_are_safe(url) {
        return Err(UnifiedError::bad_request(
            "Redirect target resolves to a private, loopback, link-local, or reserved address; redirect refused",
        ));
    }
    Ok(())
}

pub(crate) fn has_value(value: &Option<String>) -> bool {
    value
        .as_deref()
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

pub(crate) fn missing_field(kind: &str, field: &str) -> UnifiedError {
    UnifiedError::bad_request(format!(
        "Panel \"{kind}\" requires credential field \"{field}\""
    ))
}

/// Validate the generic adapter configuration.
pub(crate) fn validate_generic_config(config: &GenericPanelConfig) -> UnifiedResult<()> {
    let auth_type = config.auth_type();
    if !matches!(
        auth_type,
        "none" | "bearer" | "basic" | "api_key_header" | "api_key_query"
    ) {
        return Err(UnifiedError::bad_request(format!(
            "Generic panel auth_type \"{auth_type}\" is not supported (expected none, bearer, basic, api_key_header, or api_key_query)"
        )));
    }
    if config.endpoints.is_empty() {
        return Err(UnifiedError::bad_request(
            "Generic panel requires at least one endpoint in generic_config.endpoints",
        ));
    }
    for (collection, path) in &config.endpoints {
        if collection_from_segment(collection).is_none() {
            return Err(UnifiedError::bad_request(format!(
                "Generic panel endpoint collection \"{collection}\" is not a known collection (expected one of clients, products, services, domains, invoices, server_accounts)"
            )));
        }
        let path = path.trim();
        if !path.starts_with('/') {
            return Err(UnifiedError::bad_request(format!(
                "Generic panel endpoint for \"{collection}\" must be a path starting with \"/\" (got \"{path}\")"
            )));
        }
        if path.contains("://") || path.contains("..") {
            return Err(UnifiedError::bad_request(format!(
                "Generic panel endpoint for \"{collection}\" must be a relative path without \"..\" or a scheme"
            )));
        }
    }
    for (collection, mapping) in &config.field_mapping {
        if collection_from_segment(collection).is_none() {
            return Err(UnifiedError::bad_request(format!(
                "Generic panel field mapping collection \"{collection}\" is not a known collection"
            )));
        }
        for field in [
            mapping.id_field(),
            mapping.name_field(),
            mapping.status_field(),
        ] {
            if field.trim().is_empty() {
                return Err(UnifiedError::bad_request(format!(
                    "Generic panel field mapping for \"{collection}\" contains an empty field name"
                )));
            }
        }
    }
    Ok(())
}

/// Required credential / field set per kind. A bad connection is
/// rejected at creation time instead of failing its first API call.
pub(crate) fn validate_connection(
    kind_str: &str,
    name: &str,
    endpoint: Option<&str>,
    credentials: &PanelCredentials,
    generic_config: Option<&GenericPanelConfig>,
    allow_private: bool,
) -> UnifiedResult<PanelKind> {
    let kind = PanelKind::from_str(kind_str)
        .ok_or_else(|| UnifiedError::bad_request(format!("Unknown panel kind \"{kind_str}\"")))?;
    if name.trim().is_empty() {
        return Err(UnifiedError::bad_request("Connection name is required"));
    }
    validate_panel_endpoint(endpoint, kind.as_str(), allow_private)?;
    match kind {
        PanelKind::Whmcs => {
            if !has_value(&credentials.api_identifier) {
                return Err(missing_field("whmcs", "api_identifier"));
            }
            if !has_value(&credentials.api_secret) {
                return Err(missing_field("whmcs", "api_secret"));
            }
        }
        PanelKind::Cpanel => {
            if !has_value(&credentials.username) {
                return Err(missing_field("cpanel", "username"));
            }
            if !has_value(&credentials.api_token) {
                return Err(missing_field("cpanel", "api_token"));
            }
        }
        PanelKind::DirectAdmin => {
            if !has_value(&credentials.username) {
                return Err(missing_field("directadmin", "username"));
            }
            if !has_value(&credentials.password) && !has_value(&credentials.api_token) {
                return Err(UnifiedError::bad_request(
                    "Panel \"directadmin\" requires password or api_token (a login key)",
                ));
            }
        }
        PanelKind::Plesk => {
            if !has_value(&credentials.api_key) && !has_value(&credentials.api_token) {
                return Err(UnifiedError::bad_request(
                    "Panel \"plesk\" requires api_key (or api_token)",
                ));
            }
        }
        PanelKind::CyberPanel => {
            if !has_value(&credentials.api_token)
                && !(has_value(&credentials.username) && has_value(&credentials.password))
            {
                return Err(UnifiedError::bad_request(
                    "Panel \"cyberpanel\" requires api_token, or username + password",
                ));
            }
        }
        PanelKind::Generic => {
            let Some(config) = generic_config else {
                return Err(UnifiedError::bad_request(
                    "Panel \"generic\" requires generic_config (auth_type, endpoints, field mapping)",
                ));
            };
            validate_generic_config(config)?;
            match config.auth_type() {
                "bearer" | "api_key_header" | "api_key_query" => {
                    if !has_value(&credentials.api_token) && !has_value(&credentials.api_key) {
                        return Err(UnifiedError::bad_request(
                            "Generic panel auth requires api_token or api_key",
                        ));
                    }
                }
                "basic" => {
                    if !has_value(&credentials.username) {
                        return Err(missing_field("generic", "username"));
                    }
                    if !has_value(&credentials.password) {
                        return Err(missing_field("generic", "password"));
                    }
                }
                _ => {}
            }
        }
    }
    Ok(kind)
}

#[cfg(test)]
mod tests {
    use super::PanelCredentials;
    use super::PanelKind;
    use super::collection_from_segment;
    use super::collection_segment;
    use super::redirect_target_allowed;
    use super::validate_panel_endpoint;
    use crate::modules::panels::COLLECTIONS;
    use url::Url;

    #[test]
    fn collection_segments_round_trip() {
        for collection in COLLECTIONS {
            let segment = collection_segment(collection);
            assert_eq!(collection_from_segment(segment), Some(*collection));
        }
    }

    #[test]
    fn collection_segment_defaults_unknown_to_clients() {
        assert_eq!(collection_segment("no-such-collection"), "clients");
    }

    #[test]
    fn collection_from_segment_rejects_unknown() {
        assert_eq!(collection_from_segment("nope"), None);
    }

    #[test]
    fn kind_round_trips() {
        for kind in PanelKind::all() {
            assert_eq!(PanelKind::from_str(kind.as_str()), Some(*kind));
        }
        assert_eq!(PanelKind::from_str("nope"), None);
    }

    #[test]
    fn kind_display_names() {
        assert_eq!(PanelKind::Whmcs.display_name(), "WHMCS");
        assert_eq!(PanelKind::Generic.display_name(), "Generic panel");
    }

    #[test]
    fn all_kinds_count() {
        assert_eq!(PanelKind::all().len(), 6);
    }

    #[test]
    fn endpoint_rejects_loopback_private_and_link_local_hosts() {
        for host in [
            "http://127.0.0.1:8080/",
            "http://[::1]/",
            "http://10.0.0.5/",
            "http://172.16.0.9/",
            "http://192.168.1.10/",
            "http://169.254.169.254/",
            "http://localhost:8080/",
            "http://[::ffff:127.0.0.1]/",
        ] {
            assert!(
                validate_panel_endpoint(Some(host), "whmcs", false).is_err(),
                "{host} must be refused without the private-endpoint opt-in"
            );
        }
    }

    #[test]
    fn endpoint_allows_public_ip_and_private_with_opt_in() {
        // 93.184.216.34 is a public address: no DNS lookup needed.
        assert!(validate_panel_endpoint(Some("https://93.184.216.34/"), "whmcs", false).is_ok());
        assert!(
            validate_panel_endpoint(Some("http://127.0.0.1:8080/"), "whmcs", true).is_ok()
        );
        assert!(
            validate_panel_endpoint(Some("http://192.168.1.10/"), "whmcs", true).is_ok()
        );
    }

    #[test]
    fn endpoint_still_rejects_bad_scheme_and_embedded_credentials() {
        assert!(validate_panel_endpoint(Some("ftp://93.184.216.34/"), "whmcs", false).is_err());
        assert!(
            validate_panel_endpoint(Some("https://user:pass@93.184.216.34/"), "whmcs", false)
                .is_err()
        );
        assert!(validate_panel_endpoint(None, "whmcs", false).is_err());
    }

    #[test]
    fn redirect_target_must_stay_on_configured_public_host() {
        let same_host = Url::parse("https://93.184.216.34/other").unwrap();
        assert!(redirect_target_allowed(&same_host, "93.184.216.34", false).is_ok());
        let other_host = Url::parse("https://93.184.216.35/other").unwrap();
        assert!(redirect_target_allowed(&other_host, "93.184.216.34", false).is_err());
        let internal = Url::parse("https://127.0.0.1/other").unwrap();
        assert!(redirect_target_allowed(&internal, "127.0.0.1", false).is_err());
        assert!(redirect_target_allowed(&internal, "127.0.0.1", true).is_ok());
    }

    #[test]
    fn credentials_debug_never_prints_values() {
        let credentials = PanelCredentials {
            api_token: Some("tok-secret-value".to_string()),
            api_key: Some("key-secret-value".to_string()),
            api_identifier: Some("id-secret-value".to_string()),
            api_secret: Some("sec-secret-value".to_string()),
            username: Some("user-secret-value".to_string()),
            password: Some("pass-secret-value".to_string()),
        };
        let rendered = format!("{credentials:?}");
        for secret in [
            "tok-secret-value",
            "key-secret-value",
            "id-secret-value",
            "sec-secret-value",
            "user-secret-value",
            "pass-secret-value",
        ] {
            assert!(!rendered.contains(secret), "Debug leaked {secret}");
        }
        assert!(rendered.contains("[redacted]"));
    }
}
