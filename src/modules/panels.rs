// Panel integrations module — hosting / billing panel connectors.
//
// One hookable panel registry with a common adapter interface, so new
// panels plug in through the registry instead of scattered code. Every
// adapter is an honest, typed HTTP client: it validates its
// configuration, tests the connection against the real panel API, and
// maps panel responses onto the shared record shapes. No adapter
// pretends to be a full vendor SDK, and no adapter claims a collection
// it cannot actually fetch: the capability flags are the contract.
//
// Scope of this round (deliberately minimal, per panel):
//
//   - WHMCS: clients, products, services (client products), domains,
//     and invoices through the action-based API (`includes/api.php`
//     with identifier + secret). No write operations in this round:
//     creating or updating billing records through the panel API is
//     out of scope; this module imports and reports only.
//   - cPanel / WHM: server accounts (`listaccts`), packages
//     (`listpkgs`, exposed as products), and the domains reported by
//     `listaccts`. WHM has no billing clients / invoices API, so
//     those collections are unsupported here.
//   - DirectAdmin: users (`CMD_API_SHOW_ALL_USERS`, exposed as
//     clients and as server accounts), domains, and packages. The
//     JSON shapes returned by DirectAdmin vary by version; the parser
//     accepts an array, a `list` array, and a map of objects, and
//     reports an error when none of those shapes is present.
//   - Plesk: clients, subscriptions (exposed as services), domains,
//     and service plans (exposed as products) through the JSON API
//     (`/api/v2/...`) with an API key. Plesk has no billing invoices
//     API, so invoices are unsupported here.
//   - CyberPanel: websites (exposed as server accounts and as
//     domains) and packages (exposed as products) through the JSON
//     API. CyberPanel has no billing clients / services / invoices
//     API, so those collections are unsupported here.
//   - Generic: a configuration-driven adapter (base URL + auth type +
//     per-collection endpoint templates + response field mapping) for
//     any other panel that answers JSON over HTTP. It validates its
//     configuration, allow-lists only the configured base host, and
//     reports exactly which collections it was configured for.
//
// Credentials live only on the connection record, are never logged,
// and are never returned by any API response: read views expose only
// which credential fields are configured. Panel API hosts are
// allow-listed per connection: every request must target the host of
// the connection's own endpoint URL, and user-supplied endpoints must
// be http(s) URLs with a host and no embedded credentials. A 429 from
// a panel is surfaced as a 429 with the panel's `Retry-After` delay
// in `error.metadata`.
//
// Imported records are stored in memory per connection (same pattern
// as the provider inventory store), each with its remote id, a hash
// of the remote payload, a last-synced timestamp, and two-way sync
// state. Two-way sync never silently overwrites: when a record
// changed both locally (flagged through the API) and remotely, the
// sync reports a conflict and keeps the remote payload unless the
// record is explicitly flagged `prefer_local`, in which case the
// local version wins and the conflict is logged. Events emitted on
// the hook bus: `panel.connected`, `panel.client.synced`,
// `panel.service.synced`, `panel.sync.completed`,
// `panel.sync.failed`, plus `panel.connection.created` and
// `panel.connection.deleted` for the audit trail (add them to
// `AUDITED_EVENTS` in the audit module to record connection
// add/delete and sync runs).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::HookBus;
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Events.
// ---------------------------------------------------------------------------

/// Emitted after a connection test succeeds.
pub const PANEL_CONNECTED: &str = "panel.connected";
/// Emitted once per client record imported or updated by a sync.
pub const PANEL_CLIENT_SYNCED: &str = "panel.client.synced";
/// Emitted once per service record imported or updated by a sync.
pub const PANEL_SERVICE_SYNCED: &str = "panel.service.synced";
/// Emitted after a sync run completes (even partially).
pub const PANEL_SYNC_COMPLETED: &str = "panel.sync.completed";
/// Emitted when a sync run fails for every collection, or fails
/// before any collection could be fetched.
pub const PANEL_SYNC_FAILED: &str = "panel.sync.failed";
/// Emitted when a connection is added or deleted (audit trail).
pub const PANEL_CONNECTION_CREATED: &str = "panel.connection.created";
pub const PANEL_CONNECTION_DELETED: &str = "panel.connection.deleted";
/// Emitted when a two-way sync conflict is detected or resolved.
pub const PANEL_SYNC_CONFLICT: &str = "panel.sync.conflict";

// ---------------------------------------------------------------------------
// Limits.
// ---------------------------------------------------------------------------

/// Default HTTP timeout for panel API calls.
const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// Default connect timeout for panel API calls.
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 10;
/// Longest `Retry-After` delay honoured from a panel.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(120);
/// Maximum records fetched in one live list call per collection.
const MAX_LIST_LIMIT: u32 = 250;
/// User-Agent sent to panel APIs.
const USER_AGENT: &str = concat!("unified/", env!("CARGO_PKG_VERSION"), " panels");

/// The six importable collections, in a stable order.
pub const COLLECTIONS: [&str; 6] = [
    "clients",
    "products",
    "services",
    "domains",
    "invoices",
    "server_accounts",
];

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
    fn from_value<T: Serialize>(id: String, name: String, status: String, value: &T) -> Self {
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
#[derive(Debug, Clone, Default, Deserialize)]
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

impl PanelCredentials {
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
    fn merge(&mut self, update: &PanelCredentials) {
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
    fn id_field(&self) -> &str {
        self.id_field.as_deref().unwrap_or("id")
    }
    fn name_field(&self) -> &str {
        self.name_field.as_deref().unwrap_or("name")
    }
    fn status_field(&self) -> &str {
        self.status_field.as_deref().unwrap_or("status")
    }
}

/// Configuration for the generic panel adapter.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GenericPanelConfig {
    /// Auth type: `none`, `bearer`, `basic`, `api_key_header`, or
    /// `api_key_query`.
    #[serde(default)]
    pub auth_type: Option<String>,
    /// Header name for `api_key_header` auth (default `X-API-Key`).
    #[serde(default)]
    pub api_key_header: Option<String>,
    /// Query parameter name for `api_key_query` auth (default
    /// `api_key`).
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
    fn auth_type(&self) -> &str {
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
    fn view(&self) -> PanelConnectionView {
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

/// Validate a user-supplied panel endpoint: http(s), a host, no
/// embedded user info. The host itself is operator-configured, so
/// private / loopback hosts are allowed here — the operator chose the
/// endpoint. The same check is reused for every kind.
fn validate_panel_endpoint(endpoint: Option<&str>, kind: &str) -> UnifiedResult<Url> {
    let Some(raw) = endpoint else {
        return Err(UnifiedError::bad_request(format!(
            "Panel \"{kind}\" requires endpoint (the panel base URL)"
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

fn has_value(value: &Option<String>) -> bool {
    value.as_deref().map(|v| !v.trim().is_empty()).unwrap_or(false)
}

fn missing_field(kind: &str, field: &str) -> UnifiedError {
    UnifiedError::bad_request(format!(
        "Panel \"{kind}\" requires credential field \"{field}\""
    ))
}

/// Validate the generic adapter configuration.
fn validate_generic_config(config: &GenericPanelConfig) -> UnifiedResult<()> {
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
        for field in [mapping.id_field(), mapping.name_field(), mapping.status_field()] {
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
fn validate_connection(
    kind_str: &str,
    name: &str,
    endpoint: Option<&str>,
    credentials: &PanelCredentials,
    generic_config: Option<&GenericPanelConfig>,
) -> UnifiedResult<PanelKind> {
    let kind = PanelKind::from_str(kind_str).ok_or_else(|| {
        UnifiedError::bad_request(format!("Unknown panel kind \"{kind_str}\""))
    })?;
    if name.trim().is_empty() {
        return Err(UnifiedError::bad_request("Connection name is required"));
    }
    validate_panel_endpoint(endpoint, kind.as_str())?;
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

// ---------------------------------------------------------------------------
// In-memory stores (same pattern as the provider connection store).
// ---------------------------------------------------------------------------

static CONNECTIONS: OnceLock<DashMap<Uuid, PanelConnection>> = OnceLock::new();

fn connections_store() -> &'static DashMap<Uuid, PanelConnection> {
    CONNECTIONS.get_or_init(DashMap::new)
}

/// Direction of the last sync decision for one stored record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncDirection {
    /// First import, or the remote record changed and was applied.
    RemoteToLocal,
    /// Nothing changed on either side since the last sync.
    Unchanged,
    /// Both sides changed; the record is in conflict.
    Conflict,
    /// The local version was explicitly preferred over the remote one.
    LocalToRemote,
}

impl SyncDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RemoteToLocal => "remote_to_local",
            Self::Unchanged => "unchanged",
            Self::Conflict => "conflict",
            Self::LocalToRemote => "local_to_remote",
        }
    }
}

/// One imported record, with its two-way sync state.
#[derive(Debug, Clone, Serialize)]
pub struct SyncedRecord {
    pub connection_id: Uuid,
    pub collection: String,
    pub remote_id: String,
    pub name: String,
    pub status: String,
    /// SHA-256 hex of the canonical remote payload at last sync.
    pub hash: String,
    pub last_synced_at: DateTime<Utc>,
    pub direction: String,
    pub conflict: bool,
    /// True when a local change was flagged through the API and has
    /// not been resolved yet.
    pub local_changed: bool,
    /// True when the operator explicitly chose the local version for
    /// this record.
    pub prefer_local: bool,
    /// The last payload applied locally (remote payload, unless the
    /// local version won a conflict).
    pub payload: Value,
}

/// Key: (connection id, collection, remote id).
type RecordKey = (Uuid, String, String);

static RECORDS: OnceLock<DashMap<RecordKey, SyncedRecord>> = OnceLock::new();

fn records_store() -> &'static DashMap<RecordKey, SyncedRecord> {
    RECORDS.get_or_init(DashMap::new)
}

/// Last sync run state per connection.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PanelSyncState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default)]
    pub counts: HashMap<String, usize>,
    #[serde(default)]
    pub conflicts: u64,
    #[serde(default)]
    pub total_records: u64,
}

static SYNC_STATE: OnceLock<DashMap<Uuid, PanelSyncState>> = OnceLock::new();

fn sync_state_store() -> &'static DashMap<Uuid, PanelSyncState> {
    SYNC_STATE.get_or_init(DashMap::new)
}

// ---------------------------------------------------------------------------
// Panel registry: the plug-in point for adapters.
// ---------------------------------------------------------------------------

/// Factory that builds an adapter for one stored connection.
pub type PanelFactory = Arc<
    dyn Fn(&PanelConnection) -> UnifiedResult<Arc<dyn PanelAdapter>> + Send + Sync,
>;

pub struct PanelRegistry {
    factories: RwLock<HashMap<PanelKind, PanelFactory>>,
}

impl PanelRegistry {
    fn new() -> Self {
        Self {
            factories: RwLock::new(HashMap::new()),
        }
    }

    /// Register (or replace) the factory for a kind. This is how a new
    /// panel plugs in: call it once at startup, or from an extension,
    /// instead of editing this module.
    pub fn register_factory(&self, kind: PanelKind, factory: PanelFactory) {
        self.factories
            .write()
            .expect("panel registry lock poisoned")
            .insert(kind, factory);
    }

    pub fn build(&self, connection: &PanelConnection) -> UnifiedResult<Arc<dyn PanelAdapter>> {
        let factories = self
            .factories
            .read()
            .expect("panel registry lock poisoned");
        let factory = factories.get(&connection.kind).ok_or_else(|| {
            UnifiedError::internal(format!(
                "No panel adapter registered for kind \"{}\"",
                connection.kind.as_str()
            ))
        })?;
        factory(connection)
    }

    pub fn registered_kinds(&self) -> Vec<PanelKind> {
        let factories = self
            .factories
            .read()
            .expect("panel registry lock poisoned");
        let mut kinds: Vec<PanelKind> = factories.keys().copied().collect();
        kinds.sort_by_key(|kind| kind.as_str());
        kinds
    }
}

static REGISTRY: OnceLock<PanelRegistry> = OnceLock::new();

/// The shared registry, with the six built-in adapters registered on
/// first use.
pub fn panel_registry() -> &'static PanelRegistry {
    REGISTRY.get_or_init(|| {
        let registry = PanelRegistry::new();
        registry.register_factory(
            PanelKind::Whmcs,
            Arc::new(|connection| {
                Ok(Arc::new(WhmcsAdapter::new(connection)?) as Arc<dyn PanelAdapter>)
            }),
        );
        registry.register_factory(
            PanelKind::Cpanel,
            Arc::new(|connection| {
                Ok(Arc::new(CpanelAdapter::new(connection)?) as Arc<dyn PanelAdapter>)
            }),
        );
        registry.register_factory(
            PanelKind::DirectAdmin,
            Arc::new(|connection| {
                Ok(Arc::new(DirectAdminAdapter::new(connection)?) as Arc<dyn PanelAdapter>)
            }),
        );
        registry.register_factory(
            PanelKind::Plesk,
            Arc::new(|connection| {
                Ok(Arc::new(PleskAdapter::new(connection)?) as Arc<dyn PanelAdapter>)
            }),
        );
        registry.register_factory(
            PanelKind::CyberPanel,
            Arc::new(|connection| {
                Ok(Arc::new(CyberPanelAdapter::new(connection)?) as Arc<dyn PanelAdapter>)
            }),
        );
        registry.register_factory(
            PanelKind::Generic,
            Arc::new(|connection| {
                Ok(Arc::new(GenericAdapter::new(connection)?) as Arc<dyn PanelAdapter>)
            }),
        );
        registry
    })
}

// ---------------------------------------------------------------------------
// Common adapter interface.
// ---------------------------------------------------------------------------

/// The interface every panel adapter implements. List methods have
/// default implementations that fail with a clear "not supported"
/// error, so an adapter only implements the collections its panel
/// actually exposes; `capabilities()` is the contract callers check
/// first. `sync_all` has a default implementation that fetches every
/// supported collection and aggregates counts and errors.
#[async_trait]
pub trait PanelAdapter: Send + Sync {
    fn kind(&self) -> PanelKind;

    fn capabilities(&self) -> PanelCapabilities {
        capabilities_for(self.kind())
    }

    /// Verify the credentials against the real panel API.
    async fn test_connection(&self) -> UnifiedResult<PanelInfo>;

    async fn list_clients(&self) -> UnifiedResult<Vec<PanelClient>> {
        Err(unsupported(self.kind(), "clients"))
    }

    async fn list_products(&self) -> UnifiedResult<Vec<PanelProduct>> {
        Err(unsupported(self.kind(), "products"))
    }

    async fn list_services(&self) -> UnifiedResult<Vec<PanelService>> {
        Err(unsupported(self.kind(), "services"))
    }

    async fn list_domains(&self) -> UnifiedResult<Vec<PanelDomain>> {
        Err(unsupported(self.kind(), "domains"))
    }

    async fn list_invoices(&self) -> UnifiedResult<Vec<PanelInvoice>> {
        Err(unsupported(self.kind(), "invoices"))
    }

    async fn list_server_accounts(&self) -> UnifiedResult<Vec<PanelServerAccount>> {
        Err(unsupported(self.kind(), "server_accounts"))
    }

    /// Fetch every supported collection and return per-collection
    /// counts plus per-collection errors. A failed collection does
    /// not abort the others.
    async fn sync_all(&self) -> UnifiedResult<PanelSyncCounts> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        let mut errors: HashMap<String, String> = HashMap::new();
        let capabilities = self.capabilities();
        for collection in COLLECTIONS {
            if !capabilities.supports(collection) {
                continue;
            }
            let result: UnifiedResult<usize> = match collection {
                "clients" => self.list_clients().await.map(|items| items.len()),
                "products" => self.list_products().await.map(|items| items.len()),
                "services" => self.list_services().await.map(|items| items.len()),
                "domains" => self.list_domains().await.map(|items| items.len()),
                "invoices" => self.list_invoices().await.map(|items| items.len()),
                "server_accounts" => self
                    .list_server_accounts()
                    .await
                    .map(|items| items.len()),
                _ => Ok(0),
            };
            match result {
                Ok(count) => {
                    counts.insert(collection.to_string(), count);
                }
                Err(error) => {
                    counts.insert(collection.to_string(), 0);
                    errors.insert(collection.to_string(), error.message);
                }
            }
        }
        Ok(PanelSyncCounts {
            counts,
            errors,
            synced_at: Utc::now(),
        })
    }
}

fn unsupported(kind: PanelKind, collection: &str) -> UnifiedError {
    UnifiedError::bad_request(format!(
        "Panel \"{}\" does not support collection \"{collection}\"",
        kind.as_str()
    ))
}

// ---------------------------------------------------------------------------
// Shared HTTP helpers.
// ---------------------------------------------------------------------------

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// SHA-256 hex of the canonical JSON rendering of a payload. Used as
/// the stored record hash for change detection.
pub fn payload_hash(payload: &Value) -> String {
    hex_encode(&Sha256::digest(payload.to_string().as_bytes()))
}

fn build_client() -> UnifiedResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS))
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))
}

/// Parse a `Retry-After` header (seconds only; HTTP dates are ignored),
/// capped so a misbehaving panel cannot stall a caller indefinitely.
fn parse_retry_after(response: &reqwest::Response) -> Option<Duration> {
    let value = response.headers().get(RETRY_AFTER)?.to_str().ok()?;
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(|secs| Duration::from_secs(secs).min(MAX_RETRY_AFTER))
}

/// Map a panel HTTP response to an error. A 429 keeps its status and
/// carries the `Retry-After` delay in metadata; 401/403/404 map to the
/// matching `UnifiedError`; other failures stay generic so panel
/// response bodies (which may contain account data) are never leaked
/// into the client-visible message.
fn http_status_error(
    status: StatusCode,
    panel: &str,
    retry_after: Option<Duration>,
) -> UnifiedError {
    let message = format!("Panel \"{panel}\" request failed with HTTP {status}");
    match status {
        StatusCode::TOO_MANY_REQUESTS => {
            let mut error = UnifiedError::new(message, 429)
                .with_metadata("panel", Value::String(panel.to_string()));
            if let Some(delay) = retry_after {
                error = error
                    .with_metadata("retry_after_secs", Value::from(delay.as_secs()));
            }
            error
        }
        StatusCode::UNAUTHORIZED => UnifiedError::unauthorized(format!(
            "Panel \"{panel}\" rejected the credentials (HTTP 401)"
        )),
        StatusCode::FORBIDDEN => UnifiedError::forbidden(format!(
            "Panel \"{panel}\" refused the request (HTTP 403): the credentials lack permission for this operation"
        )),
        StatusCode::NOT_FOUND => {
            UnifiedError::not_found(format!("Panel \"{panel}\" resource not found (HTTP 404)"))
        }
        _ if status.is_client_error() => {
            UnifiedError::bad_request(message)
                .with_metadata("panel", Value::String(panel.to_string()))
        }
        _ => UnifiedError::internal(message)
            .with_metadata("panel", Value::String(panel.to_string())),
    }
}

/// Send a request and return the parsed JSON body, or a mapped error.
/// A 429 is returned as an error (with `retry_after_secs` in metadata)
/// instead of being retried here, so the caller decides.
async fn send_json(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    panel: &str,
) -> UnifiedResult<Value> {
    let response = request.send().await.map_err(|error| {
        UnifiedError::internal(format!("Panel \"{panel}\" request failed: {error}"))
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(http_status_error(status, panel, parse_retry_after(&response)));
    }
    response.json::<Value>().await.map_err(|error| {
        UnifiedError::internal(format!(
            "Panel \"{panel}\" returned an unreadable response: {error}"
        ))
    })
}

/// Send a request and return the raw text body, or a mapped error.
/// Used for panels that answer plain text / URL-encoded lists.
async fn send_text(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    panel: &str,
) -> UnifiedResult<String> {
    let response = request.send().await.map_err(|error| {
        UnifiedError::internal(format!("Panel \"{panel}\" request failed: {error}"))
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(http_status_error(status, panel, parse_retry_after(&response)));
    }
    response.text().await.map_err(|error| {
        UnifiedError::internal(format!(
            "Panel \"{panel}\" returned an unreadable response: {error}"
        ))
    })
}

/// Refuse a URL whose host differs from the connection endpoint host.
/// This is the per-connection host allow-list: a panel adapter may
/// only talk to the host the operator configured for that connection.
fn check_panel_host(url: &Url, base: &Url, panel: &str) -> UnifiedResult<()> {
    let host = url.host_str().unwrap_or("");
    let allowed = base.host_str().unwrap_or("");
    if host != allowed {
        return Err(UnifiedError::bad_request(format!(
            "Refusing request to host \"{host}\": panel \"{panel}\" is only allowed to talk to its configured host \"{allowed}\""
        )));
    }
    Ok(())
}

/// Join a base URL and a relative path without letting the path
/// escape the base (no absolute URLs, no `..` segments).
fn join_path(base: &Url, path: &str) -> UnifiedResult<Url> {
    let trimmed = path.trim();
    if !trimmed.starts_with('/') || trimmed.contains("://") || trimmed.contains("..") {
        return Err(UnifiedError::bad_request(format!(
            "Panel path \"{trimmed}\" must be a relative path starting with \"/\""
        )));
    }
    base.join(trimmed).map_err(|_| {
        UnifiedError::bad_request(format!("Cannot join panel path \"{trimmed}\" to the base URL"))
    })
}

fn bearer_header(token: &str) -> UnifiedResult<HeaderMap> {
    let value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
        UnifiedError::bad_request("Stored credential contains characters invalid in an HTTP header")
    })?;
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, value);
    Ok(headers)
}

fn api_key_header(name: &str, value: &str) -> UnifiedResult<HeaderMap> {
    let header_name = reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
        UnifiedError::bad_request(format!("Invalid API key header name \"{name}\""))
    })?;
    let header_value = HeaderValue::from_str(value).map_err(|_| {
        UnifiedError::bad_request("Stored credential contains characters invalid in an HTTP header")
    })?;
    let mut headers = HeaderMap::new();
    headers.insert(header_name, header_value);
    Ok(headers)
}

/// Normalize a panel-native status string onto the shared set.
fn normalize_status(raw: &str) -> String {
    match raw.to_ascii_lowercase().as_str() {
        "active" | "running" | "on" | "up" | "available" | "ok" | "paid" | "enabled" => {
            "active".to_string()
        }
        "suspended" | "paused" => "suspended".to_string(),
        "cancelled" | "canceled" | "terminated" | "deleted" | "closed" | "expired" => {
            "cancelled".to_string()
        }
        "pending" | "unpaid" | "incomplete" | "draft" => "pending".to_string(),
        "fraud" | "error" | "failed" => "error".to_string(),
        "" => "unknown".to_string(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// JSON value helpers.
// ---------------------------------------------------------------------------

/// A string, number, or boolean rendered as a string.
fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn json_string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn json_string_owned(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(value_to_string)
}

fn json_f64(value: &Value, key: &str) -> Option<f64> {
    match value.get(key) {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(text)) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Read a dotted field path (`user.id`, `profile.name`) from a JSON
/// object.
fn nested_value<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

/// Extract a record array from a panel response. Tries, in order: the
/// value itself when it is an array; the configured `list_field`;
/// then the well-known wrapper keys `data`, `items`, and each of the
/// caller-supplied wrapper keys (for example WHMCS `clients.client`).
fn extract_array<'a>(value: &'a Value, list_field: Option<&str>, wrappers: &[&str]) -> Vec<&'a Value> {
    if let Value::Array(items) = value {
        return items.iter().collect();
    }
    let mut candidates: Vec<&str> = Vec::new();
    if let Some(field) = list_field {
        candidates.push(field);
    }
    candidates.extend_from_slice(&["data", "items"]);
    candidates.extend_from_slice(wrappers);
    for candidate in candidates {
        if let Some(found) = nested_value(value, candidate) {
            if let Value::Array(items) = found {
                return items.iter().collect();
            }
        }
    }
    Vec::new()
}

/// Extract a record array from a response that is a map of objects
/// keyed by username / id (DirectAdmin style): each value becomes one
/// record, with the key injected as `id` when the object has none.
fn extract_map_records(value: &Value, wrappers: &[&str]) -> Vec<Value> {
    let mut target: Option<&Value> = None;
    if value.is_object() {
        target = Some(value);
    }
    for wrapper in wrappers {
        if let Some(found) = nested_value(value, wrapper) {
            if found.is_object() {
                target = Some(found);
                break;
            }
        }
    }
    let Some(Value::Object(map)) = target else {
        return Vec::new();
    };
    map.iter()
        .filter_map(|(key, item)| {
            if !item.is_object() {
                return None;
            }
            let mut record = item.clone();
            if let Value::Object(object) = &mut record {
                object
                    .entry("id".to_string())
                    .or_insert_with(|| Value::String(key.clone()));
                object
                    .entry("username".to_string())
                    .or_insert_with(|| Value::String(key.clone()));
            }
            Some(record)
        })
        .collect()
}

/// First non-empty string among several keys, or `None`.
fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| json_string_owned(value, key))
        .filter(|text| !text.trim().is_empty())
}

/// Person / client display name from the usual name fields.
fn display_name(value: &Value) -> String {
    let full = first_string(value, &["name", "fullname", "display_name"]);
    if let Some(name) = full {
        return name;
    }
    let first = first_string(value, &["firstname", "first_name"]);
    let last = first_string(value, &["lastname", "last_name"]);
    match (first, last) {
        (Some(first), Some(last)) => format!("{first} {last}"),
        (Some(first), None) => first,
        (None, Some(last)) => last,
        (None, None) => "Unknown".to_string(),
    }
}

// ---------------------------------------------------------------------------
// WHMCS adapter.
// ---------------------------------------------------------------------------

/// WHMCS action API (`<endpoint>/includes/api.php`, form-encoded,
/// `identifier` + `secret` + `action` + `responsetype=json`).
///
/// Honest scope: read-only import of clients, products, services
/// (client products), domains, and invoices. One page of up to 250
/// records per collection per sync; WHMCS pagination beyond that is
/// not walked in this round, so very large installs import the first
/// page only and the sync result reports the fetched count. No write
/// operations (no client / invoice / service creation or update).
pub struct WhmcsAdapter {
    connection: PanelConnection,
    client: reqwest::Client,
    base: Url,
}

impl WhmcsAdapter {
    pub fn new(connection: &PanelConnection) -> UnifiedResult<Self> {
        let endpoint = connection.endpoint.clone().unwrap_or_default();
        let base = validate_panel_endpoint(Some(&endpoint), "whmcs")?;
        Ok(Self {
            connection: connection.clone(),
            client: build_client()?,
            base,
        })
    }

    fn api_url(&self) -> UnifiedResult<Url> {
        let url = join_path(&self.base, "/includes/api.php")?;
        check_panel_host(&url, &self.base, "whmcs")?;
        Ok(url)
    }

    async fn call(&self, action: &str) -> UnifiedResult<Value> {
        let identifier = self
            .connection
            .credentials
            .api_identifier
            .clone()
            .unwrap_or_default();
        let secret = self
            .connection
            .credentials
            .api_secret
            .clone()
            .unwrap_or_default();
        let params: Vec<(String, String)> = vec![
            ("identifier".to_string(), identifier),
            ("secret".to_string(), secret),
            ("action".to_string(), action.to_string()),
            ("responsetype".to_string(), "json".to_string()),
            ("limitnum".to_string(), MAX_LIST_LIMIT.to_string()),
        ];
        let started = std::time::Instant::now();
        let body = send_json(
            &self.client,
            self.client.post(self.api_url()?).form(&params),
            "whmcs",
        )
        .await?;
        let _ = started;
        // WHMCS reports API-level failures inside a 200 body.
        if json_string(&body, "result").is_some_and(|result| result == "error") {
            let message = json_string(&body, "message").unwrap_or("WHMCS API error");
            return Err(UnifiedError::bad_request(format!(
                "WHMCS API action \"{action}\" failed: {message}"
            )));
        }
        Ok(body)
    }

    async fn test_with(&self, action: &str) -> UnifiedResult<PanelInfo> {
        let started = std::time::Instant::now();
        self.call(action).await?;
        Ok(PanelInfo {
            kind: "whmcs".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: None,
            detail: format!("WHMCS API action \"{action}\" succeeded"),
        })
    }
}

#[async_trait]
impl PanelAdapter for WhmcsAdapter {
    fn kind(&self) -> PanelKind {
        PanelKind::Whmcs
    }

    async fn test_connection(&self) -> UnifiedResult<PanelInfo> {
        self.test_with("GetClients").await
    }

    async fn list_clients(&self) -> UnifiedResult<Vec<PanelClient>> {
        let body = self.call("GetClients").await?;
        let items = extract_array(&body, None, &["clients.client", "clients"]);
        Ok(items
            .iter()
            .filter_map(|item| {
                let id = first_string(item, &["id", "userid"])?;
                Some(PanelClient {
                    id,
                    name: display_name(item),
                    email: json_string_owned(item, "email"),
                    company: json_string_owned(item, "companyname"),
                    status: normalize_status(
                        json_string(item, "status").unwrap_or("active"),
                    ),
                    created_at: json_string_owned(item, "datecreated"),
                })
            })
            .collect())
    }

    async fn list_products(&self) -> UnifiedResult<Vec<PanelProduct>> {
        let body = self.call("GetProducts").await?;
        let items = extract_array(&body, None, &["products.product", "products"]);
        Ok(items
            .iter()
            .filter_map(|item| {
                let id = first_string(item, &["pid", "id"])?;
                let pricing = item.get("pricing");
                let price = pricing.and_then(|pricing| {
                    pricing
                        .get("USD")
                        .and_then(|usd| json_f64(usd, "monthly"))
                        .or_else(|| json_f64(pricing, "monthly"))
                });
                Some(PanelProduct {
                    id,
                    name: first_string(item, &["name"]).unwrap_or_else(|| "Unknown".to_string()),
                    description: json_string_owned(item, "description"),
                    price,
                    currency: Some("USD".to_string()),
                    billing_cycle: None,
                    status: "active".to_string(),
                })
            })
            .collect())
    }

    async fn list_services(&self) -> UnifiedResult<Vec<PanelService>> {
        let body = self.call("GetClientsProducts").await?;
        let items = extract_array(&body, None, &["products.product", "products"]);
        Ok(items
            .iter()
            .filter_map(|item| {
                let id = first_string(item, &["id", "serviceid"])?;
                Some(PanelService {
                    id,
                    name: first_string(item, &["name", "productname"])
                        .unwrap_or_else(|| "Unknown".to_string()),
                    client_id: first_string(item, &["clientid", "userid"]),
                    product_id: first_string(item, &["pid", "productid"]),
                    domain: json_string_owned(item, "domain"),
                    status: normalize_status(
                        json_string(item, "status").unwrap_or("active"),
                    ),
                })
            })
            .collect())
    }

    async fn list_domains(&self) -> UnifiedResult<Vec<PanelDomain>> {
        let body = self.call("GetDomains").await?;
        let items = extract_array(&body, None, &["domains.domain", "domains"]);
        Ok(items
            .iter()
            .filter_map(|item| {
                let id = first_string(item, &["id", "domainid"])?;
                Some(PanelDomain {
                    id,
                    name: first_string(item, &["domainname", "domain"])
                        .unwrap_or_else(|| "Unknown".to_string()),
                    client_id: first_string(item, &["userid", "clientid"]),
                    status: normalize_status(
                        json_string(item, "status").unwrap_or("active"),
                    ),
                    expires_at: json_string_owned(item, "expirydate"),
                })
            })
            .collect())
    }

    async fn list_invoices(&self) -> UnifiedResult<Vec<PanelInvoice>> {
        let body = self.call("GetInvoices").await?;
        let items = extract_array(&body, None, &["invoices.invoice", "invoices"]);
        Ok(items
            .iter()
            .filter_map(|item| {
                let id = first_string(item, &["id", "invoiceid"])?;
                Some(PanelInvoice {
                    id,
                    client_id: first_string(item, &["userid", "clientid"]),
                    total: json_f64(item, "total"),
                    currency: json_string_owned(item, "currencycode"),
                    status: normalize_status(
                        json_string(item, "status").unwrap_or("unpaid"),
                    ),
                    due_date: json_string_owned(item, "duedate"),
                })
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// cPanel / WHM adapter.
// ---------------------------------------------------------------------------

/// WHM JSON API (`<endpoint>/json-api/<call>?api.version=1`, auth
/// header `WHM <user>:<token>`).
///
/// Honest scope: server accounts (`listaccts`), packages
/// (`listpkgs`, exposed as products), and the domains reported by
/// `listaccts` (primary domain plus addon domains). WHM exposes no
/// billing clients, services, or invoices, so those collections are
/// unsupported.
pub struct CpanelAdapter {
    connection: PanelConnection,
    client: reqwest::Client,
    base: Url,
}

impl CpanelAdapter {
    pub fn new(connection: &PanelConnection) -> UnifiedResult<Self> {
        let endpoint = connection.endpoint.clone().unwrap_or_default();
        let base = validate_panel_endpoint(Some(&endpoint), "cpanel")?;
        Ok(Self {
            connection: connection.clone(),
            client: build_client()?,
            base,
        })
    }

    fn auth_headers(&self) -> UnifiedResult<HeaderMap> {
        let user = self
            .connection
            .credentials
            .username
            .clone()
            .unwrap_or_default();
        let token = self
            .connection
            .credentials
            .api_token
            .clone()
            .unwrap_or_default();
        let value = HeaderValue::from_str(&format!("whm {user}:{token}")).map_err(|_| {
            UnifiedError::bad_request("Stored credential contains characters invalid in an HTTP header")
        })?;
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, value);
        Ok(headers)
    }

    async fn call(&self, call: &str) -> UnifiedResult<Value> {
        let url = join_path(&self.base, &format!("/json-api/{call}"))?;
        check_panel_host(&url, &self.base, "cpanel")?;
        let url = {
            let mut url = url;
            url.query_pairs_mut().append_pair("api.version", "1");
            url
        };
        send_json(
            &self.client,
            self.client.get(url).headers(self.auth_headers()?),
            "cpanel",
        )
        .await
    }

    async fn accounts(&self) -> UnifiedResult<Vec<Value>> {
        let body = self.call("listaccts").await?;
        let items = extract_array(&body, None, &["data.acct", "acct"]);
        Ok(items.into_iter().cloned().collect())
    }
}

#[async_trait]
impl PanelAdapter for CpanelAdapter {
    fn kind(&self) -> PanelKind {
        PanelKind::Cpanel
    }

    async fn test_connection(&self) -> UnifiedResult<PanelInfo> {
        let started = std::time::Instant::now();
        self.call("listaccts").await?;
        Ok(PanelInfo {
            kind: "cpanel".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: self.connection.credentials.username.clone(),
            detail: "WHM listaccts succeeded".to_string(),
        })
    }

    async fn list_products(&self) -> UnifiedResult<Vec<PanelProduct>> {
        let body = self.call("listpkgs").await?;
        let items = extract_array(&body, None, &["data.pkg", "pkg"]);
        Ok(items
            .iter()
            .filter_map(|item| {
                let name = first_string(item, &["name", "pkg"])?;
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
            .collect())
    }

    async fn list_domains(&self) -> UnifiedResult<Vec<PanelDomain>> {
        let accounts = self.accounts().await?;
        let mut domains = Vec::new();
        for account in &accounts {
            let owner = first_string(account, &["user"]);
            let mut names: Vec<String> = Vec::new();
            if let Some(domain) = first_string(account, &["domain"]) {
                names.push(domain);
            }
            if let Some(Value::Array(addons)) = account.get("addon_domains") {
                for addon in addons {
                    if let Some(name) = value_to_string(addon) {
                        names.push(name);
                    }
                }
            }
            for name in names {
                domains.push(PanelDomain {
                    id: name.clone(),
                    name,
                    client_id: owner.clone(),
                    status: normalize_status(
                        json_string(account, "suspended").map_or("active", |suspended| {
                            if suspended == "1" {
                                "suspended"
                            } else {
                                "active"
                            }
                        }),
                    ),
                    expires_at: None,
                });
            }
        }
        Ok(domains)
    }

    async fn list_server_accounts(&self) -> UnifiedResult<Vec<PanelServerAccount>> {
        let accounts = self.accounts().await?;
        Ok(accounts
            .iter()
            .filter_map(|account| {
                let username = first_string(account, &["user"])?;
                Some(PanelServerAccount {
                    id: username.clone(),
                    username,
                    domain: first_string(account, &["domain"]),
                    plan: first_string(account, &["plan"]),
                    status: if json_string(account, "suspended") == Some("1") {
                        "suspended".to_string()
                    } else {
                        "active".to_string()
                    },
                    ip: first_string(account, &["ip"]),
                })
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// DirectAdmin adapter.
// ---------------------------------------------------------------------------

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
    connection: PanelConnection,
    client: reqwest::Client,
    base: Url,
}

impl DirectAdminAdapter {
    pub fn new(connection: &PanelConnection) -> UnifiedResult<Self> {
        let endpoint = connection.endpoint.clone().unwrap_or_default();
        let base = validate_panel_endpoint(Some(&endpoint), "directadmin")?;
        Ok(Self {
            connection: connection.clone(),
            client: build_client()?,
            base,
        })
    }

    fn request(&self, path: &str) -> UnifiedResult<reqwest::RequestBuilder> {
        let url = join_path(&self.base, path)?;
        check_panel_host(&url, &self.base, "directadmin")?;
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

    async fn call(&self, path: &str) -> UnifiedResult<Value> {
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

    async fn users(&self) -> UnifiedResult<Vec<Value>> {
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
                    status: normalize_status(
                        json_string(user, "status").unwrap_or("active"),
                    ),
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
                    status: normalize_status(
                        json_string(user, "status").unwrap_or("active"),
                    ),
                    ip: first_string(user, &["ip"]),
                })
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Plesk adapter.
// ---------------------------------------------------------------------------

/// Plesk JSON API (`<endpoint>/api/v2/<resource>`, header
/// `X-API-Key: <key>`).
///
/// Honest scope: clients, subscriptions (exposed as services),
/// domains, and service plans (exposed as products). Plesk exposes
/// no billing invoices through this API, so invoices are unsupported.
pub struct PleskAdapter {
    connection: PanelConnection,
    client: reqwest::Client,
    base: Url,
}

impl PleskAdapter {
    pub fn new(connection: &PanelConnection) -> UnifiedResult<Self> {
        let endpoint = connection.endpoint.clone().unwrap_or_default();
        let base = validate_panel_endpoint(Some(&endpoint), "plesk")?;
        Ok(Self {
            connection: connection.clone(),
            client: build_client()?,
            base,
        })
    }

    fn key(&self) -> String {
        self.connection
            .credentials
            .api_key
            .clone()
            .or_else(|| self.connection.credentials.api_token.clone())
            .unwrap_or_default()
    }

    async fn call(&self, path: &str) -> UnifiedResult<Value> {
        let url = join_path(&self.base, path)?;
        check_panel_host(&url, &self.base, "plesk")?;
        let headers = api_key_header("X-API-Key", &self.key())?;
        send_json(
            &self.client,
            self.client.get(url).headers(headers),
            "plesk",
        )
        .await
    }
}

#[async_trait]
impl PanelAdapter for PleskAdapter {
    fn kind(&self) -> PanelKind {
        PanelKind::Plesk
    }

    async fn test_connection(&self) -> UnifiedResult<PanelInfo> {
        let started = std::time::Instant::now();
        self.call("/api/v2/clients").await?;
        Ok(PanelInfo {
            kind: "plesk".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: None,
            detail: "Plesk /api/v2/clients succeeded".to_string(),
        })
    }

    async fn list_clients(&self) -> UnifiedResult<Vec<PanelClient>> {
        let body = self.call("/api/v2/clients").await?;
        let items = extract_array(&body, None, &[]);
        Ok(items
            .iter()
            .filter_map(|item| {
                let id = first_string(item, &["id", "guid"])?;
                Some(PanelClient {
                    id,
                    name: display_name(item),
                    email: json_string_owned(item, "email"),
                    company: json_string_owned(item, "company"),
                    status: normalize_status(
                        json_string(item, "status").unwrap_or("active"),
                    ),
                    created_at: json_string_owned(item, "created"),
                })
            })
            .collect())
    }

    async fn list_products(&self) -> UnifiedResult<Vec<PanelProduct>> {
        let body = self.call("/api/v2/service-plans").await?;
        let items = extract_array(&body, None, &[]);
        Ok(items
            .iter()
            .filter_map(|item| {
                let id = first_string(item, &["id", "guid"])?;
                Some(PanelProduct {
                    id,
                    name: first_string(item, &["name"]).unwrap_or_else(|| "Unknown".to_string()),
                    description: json_string_owned(item, "description"),
                    price: None,
                    currency: None,
                    billing_cycle: None,
                    status: normalize_status(
                        json_string(item, "status").unwrap_or("active"),
                    ),
                })
            })
            .collect())
    }

    async fn list_services(&self) -> UnifiedResult<Vec<PanelService>> {
        let body = self.call("/api/v2/subscriptions").await?;
        let items = extract_array(&body, None, &[]);
        Ok(items
            .iter()
            .filter_map(|item| {
                let id = first_string(item, &["id", "guid"])?;
                Some(PanelService {
                    id,
                    name: first_string(item, &["name"]).unwrap_or_else(|| "Unknown".to_string()),
                    client_id: first_string(item, &["ownerId", "client_id", "owner_id"]),
                    product_id: first_string(item, &["planId", "plan_id", "service_plan_id"]),
                    domain: first_string(item, &["name", "domain"]),
                    status: normalize_status(
                        json_string(item, "status").unwrap_or("active"),
                    ),
                })
            })
            .collect())
    }

    async fn list_domains(&self) -> UnifiedResult<Vec<PanelDomain>> {
        let body = self.call("/api/v2/domains").await?;
        let items = extract_array(&body, None, &[]);
        Ok(items
            .iter()
            .filter_map(|item| {
                let id = first_string(item, &["id", "guid"])?;
                Some(PanelDomain {
                    id,
                    name: first_string(item, &["name", "asciiName"])
                        .unwrap_or_else(|| "Unknown".to_string()),
                    client_id: first_string(item, &["ownerId", "client_id", "owner_id"]),
                    status: normalize_status(
                        json_string(item, "status").unwrap_or("active"),
                    ),
                    expires_at: None,
                })
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// CyberPanel adapter.
// ---------------------------------------------------------------------------

/// CyberPanel JSON API (`<endpoint>/api/<call>`, POST with a JSON
/// body). Authentication is the API token: it is sent as a Bearer
/// header and as the JSON `token` field, because CyberPanel versions
/// differ on which one they read; when no token is configured, the
/// admin username and password are sent as `adminUser` /
/// `adminPass` instead.
///
/// Honest scope: websites (`listWebsites`, exposed as server accounts
/// and as domains) and packages (`listPackages`, exposed as
/// products). CyberPanel exposes no billing clients, services, or
/// invoices through this API, so those collections are unsupported.
pub struct CyberPanelAdapter {
    connection: PanelConnection,
    client: reqwest::Client,
    base: Url,
}

impl CyberPanelAdapter {
    pub fn new(connection: &PanelConnection) -> UnifiedResult<Self> {
        let endpoint = connection.endpoint.clone().unwrap_or_default();
        let base = validate_panel_endpoint(Some(&endpoint), "cyberpanel")?;
        Ok(Self {
            connection: connection.clone(),
            client: build_client()?,
            base,
        })
    }

    async fn call(&self, call: &str) -> UnifiedResult<Value> {
        let url = join_path(&self.base, &format!("/api/{call}"))?;
        check_panel_host(&url, &self.base, "cyberpanel")?;
        let mut body = serde_json::Map::new();
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(token) = self.connection.credentials.api_token.clone() {
            if !token.trim().is_empty() {
                headers.extend(bearer_header(&token)?);
                body.insert("token".to_string(), Value::String(token));
            }
        }
        if body.is_empty() {
            if let Some(user) = self.connection.credentials.username.clone() {
                body.insert("adminUser".to_string(), Value::String(user));
            }
            if let Some(password) = self.connection.credentials.password.clone() {
                body.insert("adminPass".to_string(), Value::String(password));
            }
        }
        send_json(
            &self.client,
            self.client.post(url).headers(headers).json(&Value::Object(body)),
            "cyberpanel",
        )
        .await
    }

    async fn websites(&self) -> UnifiedResult<Vec<Value>> {
        let body = self.call("listWebsites").await?;
        let items = extract_array(&body, None, &["websites", "data"]);
        Ok(items.into_iter().cloned().collect())
    }
}

#[async_trait]
impl PanelAdapter for CyberPanelAdapter {
    fn kind(&self) -> PanelKind {
        PanelKind::CyberPanel
    }

    async fn test_connection(&self) -> UnifiedResult<PanelInfo> {
        let started = std::time::Instant::now();
        self.call("listPackages").await?;
        Ok(PanelInfo {
            kind: "cyberpanel".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: self.connection.credentials.username.clone(),
            detail: "CyberPanel listPackages succeeded".to_string(),
        })
    }

    async fn list_products(&self) -> UnifiedResult<Vec<PanelProduct>> {
        let body = self.call("listPackages").await?;
        let items = extract_array(&body, None, &["packages", "data"]);
        Ok(items
            .iter()
            .filter_map(|item| {
                let name = first_string(item, &["packageName", "name", "package"])?;
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
            .collect())
    }

    async fn list_domains(&self) -> UnifiedResult<Vec<PanelDomain>> {
        let websites = self.websites().await?;
        Ok(websites
            .iter()
            .filter_map(|site| {
                let domain = first_string(site, &["domain", "website", "name"])?;
                Some(PanelDomain {
                    id: domain.clone(),
                    name: domain,
                    client_id: first_string(site, &["admin", "owner"]),
                    status: normalize_status(
                        json_string(site, "state").unwrap_or("active"),
                    ),
                    expires_at: None,
                })
            })
            .collect())
    }

    async fn list_server_accounts(&self) -> UnifiedResult<Vec<PanelServerAccount>> {
        let websites = self.websites().await?;
        Ok(websites
            .iter()
            .filter_map(|site| {
                let domain = first_string(site, &["domain", "website", "name"])?;
                Some(PanelServerAccount {
                    id: domain.clone(),
                    username: first_string(site, &["admin", "owner"])
                        .unwrap_or_else(|| domain.clone()),
                    domain: Some(domain),
                    plan: first_string(site, &["package", "packageName"]),
                    status: normalize_status(
                        json_string(site, "state").unwrap_or("active"),
                    ),
                    ip: first_string(site, &["ip", "ipAddress"]),
                })
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Generic adapter.
// ---------------------------------------------------------------------------

/// Configuration-driven adapter for any panel that answers JSON over
/// HTTP: the connection's `generic_config` supplies the auth type, a
/// per-collection endpoint path, and the response field mapping.
/// Only collections with a configured endpoint are supported, and
/// only the configured base host is ever contacted.
pub struct GenericAdapter {
    connection: PanelConnection,
    client: reqwest::Client,
    base: Url,
    config: GenericPanelConfig,
}

impl GenericAdapter {
    pub fn new(connection: &PanelConnection) -> UnifiedResult<Self> {
        let endpoint = connection.endpoint.clone().unwrap_or_default();
        let base = validate_panel_endpoint(Some(&endpoint), "generic")?;
        let config = connection.generic_config.clone().ok_or_else(|| {
            UnifiedError::bad_request("Panel \"generic\" requires generic_config")
        })?;
        validate_generic_config(&config)?;
        Ok(Self {
            connection: connection.clone(),
            client: build_client()?,
            base,
            config,
        })
    }

    fn auth_headers(&self) -> UnifiedResult<HeaderMap> {
        let mut headers = HeaderMap::new();
        match self.config.auth_type() {
            "bearer" => {
                let token = self
                    .connection
                    .credentials
                    .api_token
                    .clone()
                    .or_else(|| self.connection.credentials.api_key.clone())
                    .unwrap_or_default();
                headers.extend(bearer_header(&token)?);
            }
            "api_key_header" => {
                let name = self
                    .config
                    .api_key_header
                    .clone()
                    .unwrap_or_else(|| "X-API-Key".to_string());
                let value = self
                    .connection
                    .credentials
                    .api_key
                    .clone()
                    .or_else(|| self.connection.credentials.api_token.clone())
                    .unwrap_or_default();
                headers.extend(api_key_header(&name, &value)?);
            }
            _ => {}
        }
        Ok(headers)
    }

    async fn fetch(&self, collection: &str) -> UnifiedResult<Vec<RemoteRecord>> {
        let path = self.config.endpoints.get(collection).ok_or_else(|| {
            unsupported(PanelKind::Generic, collection)
        })?;
        let mut url = join_path(&self.base, path)?;
        check_panel_host(&url, &self.base, "generic")?;
        if self.config.auth_type() == "api_key_query" {
            let param = self
                .config
                .api_key_param
                .clone()
                .unwrap_or_else(|| "api_key".to_string());
            let value = self
                .connection
                .credentials
                .api_key
                .clone()
                .or_else(|| self.connection.credentials.api_token.clone())
                .unwrap_or_default();
            url.query_pairs_mut().append_pair(&param, &value);
        }
        let mut request = self.client.get(url).headers(self.auth_headers()?);
        if self.config.auth_type() == "basic" {
            request = request.basic_auth(
                self.connection.credentials.username.clone().unwrap_or_default(),
                self.connection.credentials.password.clone(),
            );
        }
        let body = send_json(&self.client, request, "generic").await?;
        let mapping = self
            .config
            .field_mapping
            .get(collection)
            .cloned()
            .unwrap_or_default();
        let items = extract_array(&body, self.config.list_field.as_deref(), &[collection]);
        let mut records = Vec::new();
        for item in items {
            let Some(id_value) = nested_value(item, mapping.id_field()) else {
                continue;
            };
            let Some(remote_id) = value_to_string(id_value) else {
                continue;
            };
            if remote_id.trim().is_empty() {
                continue;
            }
            let name = nested_value(item, mapping.name_field())
                .and_then(value_to_string)
                .unwrap_or_else(|| remote_id.clone());
            let status = nested_value(item, mapping.status_field())
                .and_then(value_to_string)
                .map(|status| normalize_status(&status))
                .unwrap_or_else(|| "unknown".to_string());
            records.push(RemoteRecord {
                remote_id,
                name,
                status,
                payload: item.clone(),
            });
        }
        Ok(records)
    }

    fn to_clients(records: Vec<RemoteRecord>) -> Vec<PanelClient> {
        records
            .into_iter()
            .map(|record| PanelClient {
                id: record.remote_id,
                name: record.name,
                email: json_string_owned(&record.payload, "email"),
                company: json_string_owned(&record.payload, "company"),
                status: record.status,
                created_at: json_string_owned(&record.payload, "created_at"),
            })
            .collect()
    }

    fn to_products(records: Vec<RemoteRecord>) -> Vec<PanelProduct> {
        records
            .into_iter()
            .map(|record| PanelProduct {
                id: record.remote_id,
                name: record.name,
                description: json_string_owned(&record.payload, "description"),
                price: json_f64(&record.payload, "price"),
                currency: json_string_owned(&record.payload, "currency"),
                billing_cycle: json_string_owned(&record.payload, "billing_cycle"),
                status: record.status,
            })
            .collect()
    }

    fn to_services(records: Vec<RemoteRecord>) -> Vec<PanelService> {
        records
            .into_iter()
            .map(|record| PanelService {
                id: record.remote_id,
                name: record.name,
                client_id: first_string(&record.payload, &["client_id", "clientId"]),
                product_id: first_string(&record.payload, &["product_id", "productId"]),
                domain: json_string_owned(&record.payload, "domain"),
                status: record.status,
            })
            .collect()
    }

    fn to_domains(records: Vec<RemoteRecord>) -> Vec<PanelDomain> {
        records
            .into_iter()
            .map(|record| PanelDomain {
                id: record.remote_id,
                name: record.name,
                client_id: first_string(&record.payload, &["client_id", "clientId"]),
                status: record.status,
                expires_at: json_string_owned(&record.payload, "expires_at"),
            })
            .collect()
    }

    fn to_invoices(records: Vec<RemoteRecord>) -> Vec<PanelInvoice> {
        records
            .into_iter()
            .map(|record| PanelInvoice {
                id: record.remote_id,
                client_id: first_string(&record.payload, &["client_id", "clientId"]),
                total: json_f64(&record.payload, "total"),
                currency: json_string_owned(&record.payload, "currency"),
                status: record.status,
                due_date: json_string_owned(&record.payload, "due_date"),
            })
            .collect()
    }

    fn to_server_accounts(records: Vec<RemoteRecord>) -> Vec<PanelServerAccount> {
        records
            .into_iter()
            .map(|record| PanelServerAccount {
                id: record.remote_id.clone(),
                username: first_string(&record.payload, &["username", "user"])
                    .unwrap_or_else(|| record.remote_id.clone()),
                domain: json_string_owned(&record.payload, "domain"),
                plan: first_string(&record.payload, &["plan", "package"]),
                status: record.status,
                ip: json_string_owned(&record.payload, "ip"),
            })
            .collect()
    }
}

#[async_trait]
impl PanelAdapter for GenericAdapter {
    fn kind(&self) -> PanelKind {
        PanelKind::Generic
    }

    fn capabilities(&self) -> PanelCapabilities {
        generic_capabilities(&self.config)
    }

    async fn test_connection(&self) -> UnifiedResult<PanelInfo> {
        let started = std::time::Instant::now();
        let Some(collection) = self.capabilities().supported_collections().into_iter().next()
        else {
            return Err(UnifiedError::bad_request(
                "Generic panel has no configured endpoints to test against",
            ));
        };
        self.fetch(collection).await?;
        Ok(PanelInfo {
            kind: "generic".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: None,
            detail: format!("Generic panel endpoint for \"{collection}\" succeeded"),
        })
    }

    async fn list_clients(&self) -> UnifiedResult<Vec<PanelClient>> {
        Ok(Self::to_clients(self.fetch("clients").await?))
    }

    async fn list_products(&self) -> UnifiedResult<Vec<PanelProduct>> {
        Ok(Self::to_products(self.fetch("products").await?))
    }

    async fn list_services(&self) -> UnifiedResult<Vec<PanelService>> {
        Ok(Self::to_services(self.fetch("services").await?))
    }

    async fn list_domains(&self) -> UnifiedResult<Vec<PanelDomain>> {
        Ok(Self::to_domains(self.fetch("domains").await?))
    }

    async fn list_invoices(&self) -> UnifiedResult<Vec<PanelInvoice>> {
        Ok(Self::to_invoices(self.fetch("invoices").await?))
    }

    async fn list_server_accounts(&self) -> UnifiedResult<Vec<PanelServerAccount>> {
        Ok(Self::to_server_accounts(
            self.fetch("server_accounts").await?,
        ))
    }
}

// ---------------------------------------------------------------------------
// Live fetch dispatch and module-level sync.
// ---------------------------------------------------------------------------

/// Fetch one collection live from an adapter, as generic records.
async fn fetch_collection(
    adapter: &Arc<dyn PanelAdapter>,
    collection: &str,
) -> UnifiedResult<Vec<RemoteRecord>> {
    match collection {
        "clients" => Ok(adapter
            .list_clients()
            .await?
            .into_iter()
            .map(|item| {
                RemoteRecord::from_value(item.id.clone(), item.name.clone(), item.status.clone(), &item)
            })
            .collect()),
        "products" => Ok(adapter
            .list_products()
            .await?
            .into_iter()
            .map(|item| {
                RemoteRecord::from_value(item.id.clone(), item.name.clone(), item.status.clone(), &item)
            })
            .collect()),
        "services" => Ok(adapter
            .list_services()
            .await?
            .into_iter()
            .map(|item| {
                RemoteRecord::from_value(item.id.clone(), item.name.clone(), item.status.clone(), &item)
            })
            .collect()),
        "domains" => Ok(adapter
            .list_domains()
            .await?
            .into_iter()
            .map(|item| {
                RemoteRecord::from_value(item.id.clone(), item.name.clone(), item.status.clone(), &item)
            })
            .collect()),
        "invoices" => Ok(adapter
            .list_invoices()
            .await?
            .into_iter()
            .map(|item| {
                RemoteRecord::from_value(item.id.clone(), item.name.clone(), item.status.clone(), &item)
            })
            .collect()),
        "server_accounts" => Ok(adapter
            .list_server_accounts()
            .await?
            .into_iter()
            .map(|item| {
                RemoteRecord::from_value(item.id.clone(), item.username.clone(), item.status.clone(), &item)
            })
            .collect()),
        _ => Err(UnifiedError::bad_request(format!(
            "Unknown collection \"{collection}\""
        ))),
    }
}

/// Outcome of syncing one collection into the in-memory store.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CollectionSyncOutcome {
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub conflicts: usize,
    /// Records whose local version explicitly won over the remote one.
    pub local_wins: usize,
}

/// Result of a module-level sync run (one or all collections).
#[derive(Debug, Clone, Serialize)]
pub struct PanelSyncResult {
    pub connection_id: Uuid,
    pub kind: String,
    pub synced_at: DateTime<Utc>,
    /// Records fetched per collection.
    pub counts: HashMap<String, usize>,
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub conflicts: usize,
    pub local_wins: usize,
    /// Per-collection fetch errors (empty when every collection
    /// succeeded). A run with errors for some collections still
    /// stores the collections that succeeded.
    pub errors: HashMap<String, String>,
    /// True when every supported collection synced without error.
    pub success: bool,
}

fn emit_record_synced(
    ctx: &Arc<AppContext>,
    connection: &PanelConnection,
    collection: &str,
    record: &RemoteRecord,
) {
    let event = match collection {
        "clients" => Some(PANEL_CLIENT_SYNCED),
        "services" => Some(PANEL_SERVICE_SYNCED),
        _ => None,
    };
    if let Some(event) = event {
        ctx.hooks.emit(
            event,
            serde_json::json!({
                "actor": "api",
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "resource": format!("panel-{collection}:{}", record.remote_id),
                "remote_id": record.remote_id,
                "name": record.name,
            }),
        );
    }
}

/// Apply live records for one collection to the in-memory store,
/// resolving two-way state per record. Never silently overwrites a
/// locally changed record: a remote change to a locally changed
/// record becomes a conflict (the stored payload is kept) unless the
/// record is flagged `prefer_local`, in which case the local version
/// wins and the conflict is counted as a local win.
fn apply_records(
    ctx: &Arc<AppContext>,
    connection: &PanelConnection,
    collection: &str,
    records: Vec<RemoteRecord>,
) -> CollectionSyncOutcome {
    let mut outcome = CollectionSyncOutcome::default();
    let now = Utc::now();
    for record in &records {
        let hash = payload_hash(&record.payload);
        let key: RecordKey = (
            connection.id,
            collection.to_string(),
            record.remote_id.clone(),
        );
        match records_store().get_mut(&key) {
            None => {
                records_store().insert(
                    key,
                    SyncedRecord {
                        connection_id: connection.id,
                        collection: collection.to_string(),
                        remote_id: record.remote_id.clone(),
                        name: record.name.clone(),
                        status: record.status.clone(),
                        hash,
                        last_synced_at: now,
                        direction: SyncDirection::RemoteToLocal.as_str().to_string(),
                        conflict: false,
                        local_changed: false,
                        prefer_local: false,
                        payload: record.payload.clone(),
                    },
                );
                outcome.inserted += 1;
                emit_record_synced(ctx, connection, collection, record);
            }
            Some(mut stored) => {
                if stored.hash == hash && !stored.local_changed {
                    stored.last_synced_at = now;
                    stored.direction = SyncDirection::Unchanged.as_str().to_string();
                    stored.conflict = false;
                    outcome.unchanged += 1;
                } else if stored.hash == hash && stored.local_changed {
                    // The remote record matches the last synced
                    // payload, but a local change is still flagged:
                    // keep the flag and the stored payload so the
                    // local change is not lost.
                    stored.last_synced_at = now;
                    stored.direction = SyncDirection::LocalToRemote.as_str().to_string();
                    outcome.unchanged += 1;
                } else if stored.local_changed && stored.prefer_local {
                    // Local version explicitly wins: keep the stored
                    // payload, clear the change flag, log the win.
                    stored.local_changed = false;
                    stored.conflict = false;
                    stored.last_synced_at = now;
                    stored.direction = SyncDirection::LocalToRemote.as_str().to_string();
                    outcome.local_wins += 1;
                } else if stored.local_changed {
                    // Both sides changed and the local side was not
                    // explicitly preferred: conflict, remote payload
                    // is NOT applied.
                    stored.conflict = true;
                    stored.last_synced_at = now;
                    stored.direction = SyncDirection::Conflict.as_str().to_string();
                    outcome.conflicts += 1;
                    ctx.hooks.emit(
                        PANEL_SYNC_CONFLICT,
                        serde_json::json!({
                            "actor": "sync",
                            "connection_id": connection.id,
                            "kind": connection.kind.as_str(),
                            "resource": format!("panel-{collection}:{}", record.remote_id),
                            "collection": collection,
                            "remote_id": record.remote_id,
                        }),
                    );
                } else {
                    stored.hash = hash;
                    stored.name = record.name.clone();
                    stored.status = record.status.clone();
                    stored.payload = record.payload.clone();
                    stored.last_synced_at = now;
                    stored.direction = SyncDirection::RemoteToLocal.as_str().to_string();
                    stored.conflict = false;
                    outcome.updated += 1;
                    emit_record_synced(ctx, connection, collection, record);
                }
            }
        }
    }
    outcome
}

/// Sync one collection: fetch live, apply to the store, and return
/// the outcome. A fetch failure is returned as an error and nothing
/// is stored for that collection.
async fn sync_one_collection(
    ctx: &Arc<AppContext>,
    connection: &PanelConnection,
    collection: &str,
) -> UnifiedResult<CollectionSyncOutcome> {
    let adapter = panel_registry().build(connection)?;
    if !adapter.capabilities().supports(collection) {
        return Err(unsupported(connection.kind, collection));
    }
    let records = fetch_collection(&adapter, collection).await?;
    Ok(apply_records(ctx, connection, collection, records))
}

/// Sync every supported collection for a connection, collecting
/// per-collection counts and errors. Emits `panel.sync.completed`
/// when at least one collection succeeded and `panel.sync.failed`
/// when every collection failed. Updates the per-connection sync
/// state and the connection's `last_synced_at`.
async fn sync_all_collections(
    ctx: &Arc<AppContext>,
    connection: &PanelConnection,
) -> PanelSyncResult {
    let adapter = match panel_registry().build(connection) {
        Ok(adapter) => Some(adapter),
        Err(error) => {
            ctx.hooks.emit(
                PANEL_SYNC_FAILED,
                serde_json::json!({
                    "actor": "sync",
                    "connection_id": connection.id,
                    "kind": connection.kind.as_str(),
                    "error": error.message,
                }),
            );
            let mut errors = HashMap::new();
            errors.insert("adapter".to_string(), error.message);
            return PanelSyncResult {
                connection_id: connection.id,
                kind: connection.kind.as_str().to_string(),
                synced_at: Utc::now(),
                counts: HashMap::new(),
                inserted: 0,
                updated: 0,
                unchanged: 0,
                conflicts: 0,
                local_wins: 0,
                errors,
                success: false,
            };
        }
    };
    let adapter = adapter.expect("adapter checked above");
    let capabilities = adapter.capabilities();
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut errors: HashMap<String, String> = HashMap::new();
    let mut total = CollectionSyncOutcome::default();
    for collection in COLLECTIONS {
        if !capabilities.supports(collection) {
            continue;
        }
        match fetch_collection(&adapter, collection).await {
            Ok(records) => {
                counts.insert(collection.to_string(), records.len());
                let outcome = apply_records(ctx, connection, collection, records);
                total.inserted += outcome.inserted;
                total.updated += outcome.updated;
                total.unchanged += outcome.unchanged;
                total.conflicts += outcome.conflicts;
                total.local_wins += outcome.local_wins;
            }
            Err(error) => {
                counts.insert(collection.to_string(), 0);
                errors.insert(collection.to_string(), error.message);
            }
        }
    }
    let now = Utc::now();
    let success = errors.is_empty();
    let any_success = COLLECTIONS.iter().any(|collection| {
        capabilities.supports(collection) && !errors.contains_key(*collection)
    });
    if let Some(mut stored) = connections_store().get_mut(&connection.id) {
        stored.last_synced_at = Some(now);
        stored.updated_at = now;
    }
    let total_records = records_store()
        .iter()
        .filter(|entry| entry.key().0 == connection.id)
        .count() as u64;
    sync_state_store().insert(
        connection.id,
        PanelSyncState {
            last_synced_at: Some(now),
            last_status: Some(if success {
                "ok".to_string()
            } else if any_success {
                "partial".to_string()
            } else {
                "failed".to_string()
            }),
            last_error: errors.values().next().cloned(),
            counts: counts.clone(),
            conflicts: total.conflicts as u64,
            total_records,
        },
    );
    let result = PanelSyncResult {
        connection_id: connection.id,
        kind: connection.kind.as_str().to_string(),
        synced_at: now,
        counts,
        inserted: total.inserted,
        updated: total.updated,
        unchanged: total.unchanged,
        conflicts: total.conflicts,
        local_wins: total.local_wins,
        errors,
        success,
    };
    if any_success {
        ctx.hooks.emit(
            PANEL_SYNC_COMPLETED,
            serde_json::json!({
                "actor": "sync",
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "inserted": result.inserted,
                "updated": result.updated,
                "unchanged": result.unchanged,
                "conflicts": result.conflicts,
                "errors": result.errors,
            }),
        );
    } else {
        ctx.hooks.emit(
            PANEL_SYNC_FAILED,
            serde_json::json!({
                "actor": "sync",
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "errors": result.errors,
            }),
        );
    }
    result
}

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct CreateConnectionRequest {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub credentials: PanelCredentials,
    #[serde(default)]
    pub generic_config: Option<GenericPanelConfig>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UpdateConnectionRequest {
    pub name: Option<String>,
    pub endpoint: Option<String>,
    pub credentials: Option<PanelCredentials>,
    pub generic_config: Option<GenericPanelConfig>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MarkLocalChangedRequest {
    #[serde(default)]
    pub prefer_local: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResolveConflictRequest {
    pub resolution: String,
}

// ---------------------------------------------------------------------------
// Views.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct PanelKindView {
    kind: String,
    display_name: String,
    capabilities: PanelCapabilities,
    required_credentials: Vec<String>,
    endpoint_required: bool,
}

// ---------------------------------------------------------------------------
// Shared handler helpers.
// ---------------------------------------------------------------------------

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

fn find_connection(id: Uuid) -> UnifiedResult<PanelConnection> {
    connections_store()
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Panel connection \"{id}\" not found")))
}

fn require_enabled(connection: &PanelConnection) -> UnifiedResult<()> {
    if connection.enabled {
        Ok(())
    } else {
        Err(UnifiedError::conflict(format!(
            "Panel connection \"{}\" is disabled",
            connection.name
        )))
    }
}

fn filter_records(mut records: Vec<SyncedRecord>, params: &PaginationParams) -> Vec<SyncedRecord> {
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        records.retain(|record| {
            record.name.to_lowercase().contains(&needle)
                || record.remote_id.to_lowercase().contains(&needle)
                || record.status.to_lowercase().contains(&needle)
        });
    }
    if let Some(status) = params.filters.get("status") {
        records.retain(|record| record.status == *status);
    }
    if let Some(direction) = params.filters.get("direction") {
        records.retain(|record| record.direction == *direction);
    }
    if let Some(conflict) = params.filters.get("conflict") {
        let wanted = conflict == "true";
        records.retain(|record| record.conflict == wanted);
    }
    let sort = params.allowed_sort(&["name", "status", "last_synced_at", "remote_id"], "name");
    match sort.as_str() {
        "status" => records.sort_by(|a, b| a.status.cmp(&b.status).then_with(|| a.name.cmp(&b.name))),
        "last_synced_at" => records
            .sort_by(|a, b| a.last_synced_at.cmp(&b.last_synced_at).then_with(|| a.name.cmp(&b.name))),
        "remote_id" => records.sort_by(|a, b| a.remote_id.cmp(&b.remote_id)),
        _ => records.sort_by(|a, b| a.name.cmp(&b.name)),
    }
    if params.normalized().order == "desc" {
        records.reverse();
    }
    records
}

fn stored_records(connection_id: Uuid, collection: &str) -> Vec<SyncedRecord> {
    records_store()
        .iter()
        .filter(|entry| entry.key().0 == connection_id && entry.key().1 == collection)
        .map(|entry| entry.value().clone())
        .collect()
}

fn list_collection_records(
    connection_id: Uuid,
    collection: &str,
    params: &PaginationParams,
) -> UnifiedResult<Value> {
    let connection = find_connection(connection_id)?;
    let adapter = panel_registry().build(&connection)?;
    if !adapter.capabilities().supports(collection) {
        return Err(unsupported(connection.kind, collection));
    }
    let records = filter_records(stored_records(connection_id, collection), params);
    let mut page = paginate(records, params).to_json();
    if let Value::Object(map) = &mut page {
        map.insert(
            "connection_id".to_string(),
            Value::String(connection_id.to_string()),
        );
        map.insert(
            "collection".to_string(),
            Value::String(collection.to_string()),
        );
        let state = sync_state_store().get(&connection_id);
        map.insert(
            "last_synced_at".to_string(),
            state
                .as_ref()
                .and_then(|state| state.last_synced_at)
                .map(|time| Value::String(time.to_rfc3339()))
                .unwrap_or(Value::Null),
        );
        map.insert(
            "sync_status".to_string(),
            state
                .as_ref()
                .and_then(|state| state.last_status.clone())
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
    }
    Ok(page)
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

/// Catalog of panel kinds and their capabilities.
async fn list_kinds(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    let registered = panel_registry().registered_kinds();
    let kinds: Vec<PanelKind> = PanelKind::all()
        .iter()
        .copied()
        .filter(|kind| registered.contains(kind))
        .collect();
    let result: UnifiedResult<Vec<PanelKindView>> = Ok(kinds
        .iter()
        .map(|kind| PanelKindView {
            kind: kind.as_str().to_string(),
            display_name: kind.display_name().to_string(),
            capabilities: capabilities_for(*kind),
            required_credentials: kind
                .required_credential_fields()
                .iter()
                .map(|field| field.to_string())
                .collect(),
            endpoint_required: true,
        })
        .collect());
    json::respond(result, debug)
}

async fn create_connection(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateConnectionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PanelConnectionView> = (|| {
        let kind = validate_connection(
            &body.kind,
            &body.name,
            body.endpoint.as_deref(),
            &body.credentials,
            body.generic_config.as_ref(),
        )?;
        let now = Utc::now();
        let connection = PanelConnection {
            id: Uuid::now_v7(),
            name: body.name.trim().to_string(),
            kind,
            endpoint: body
                .endpoint
                .as_ref()
                .map(|endpoint| endpoint.trim().to_string())
                .filter(|endpoint| !endpoint.is_empty()),
            credentials: body.credentials.clone(),
            generic_config: body.generic_config.clone(),
            enabled: body.enabled,
            last_tested_at: None,
            last_status: None,
            last_synced_at: None,
            created_at: now,
            updated_at: now,
        };
        // Validate the adapter can be built from the stored values
        // before accepting the connection.
        panel_registry().build(&connection)?;
        let view = connection.view();
        connections_store().insert(connection.id, connection.clone());
        ctx.hooks.emit(
            PANEL_CONNECTION_CREATED,
            serde_json::json!({
                "actor": "api",
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "resource": format!("panel-connection:{}", connection.id),
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
        let mut items: Vec<PanelConnectionView> = connections_store()
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
    let result: UnifiedResult<PanelConnectionView> =
        find_connection(id.into_inner()).map(|connection| connection.view());
    json::respond(result, debug)
}

async fn update_connection(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    body: web::Json<UpdateConnectionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PanelConnectionView> = (|| {
        let id = id.into_inner();
        let mut connection = find_connection(id)?;
        if let Some(name) = &body.name {
            connection.name = name.trim().to_string();
        }
        if let Some(endpoint) = &body.endpoint {
            connection.endpoint = Some(endpoint.trim().to_string());
        }
        if let Some(credentials) = &body.credentials {
            connection.credentials.merge(credentials);
        }
        if let Some(config) = &body.generic_config {
            connection.generic_config = Some(config.clone());
        }
        if let Some(enabled) = body.enabled {
            connection.enabled = enabled;
        }
        // Re-validate the merged connection before storing it.
        let kind = validate_connection(
            connection.kind.as_str(),
            &connection.name,
            connection.endpoint.as_deref(),
            &connection.credentials,
            connection.generic_config.as_ref(),
        )?;
        connection.kind = kind;
        connection.updated_at = Utc::now();
        panel_registry().build(&connection)?;
        let view = connection.view();
        connections_store().insert(id, connection);
        Ok(view)
    })();
    json::respond(result, debug)
}

async fn delete_connection(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        let connection = find_connection(id)?;
        connections_store().remove(&id);
        sync_state_store().remove(&id);
        records_store().retain(|key, _| key.0 != id);
        ctx.hooks.emit(
            PANEL_CONNECTION_DELETED,
            serde_json::json!({
                "actor": "api",
                "connection_id": id,
                "kind": connection.kind.as_str(),
                "resource": format!("panel-connection:{id}"),
                "name": connection.name,
            }),
        );
        Ok(serde_json::json!({ "id": id, "deleted": true }))
    })();
    json::respond(result, debug)
}

async fn test_connection(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PanelInfo> = (|| async {
        let connection = find_connection(id.into_inner())?;
        require_enabled(&connection)?;
        let adapter = panel_registry().build(&connection)?;
        let info = adapter.test_connection().await?;
        if let Some(mut stored) = connections_store().get_mut(&connection.id) {
            stored.last_tested_at = Some(Utc::now());
            stored.last_status = Some("ok".to_string());
            stored.updated_at = Utc::now();
        }
        ctx.hooks.emit(
            PANEL_CONNECTED,
            serde_json::json!({
                "actor": "api",
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "resource": format!("panel-connection:{}", connection.id),
                "latency_ms": info.latency_ms,
            }),
        );
        Ok(info)
    })()
    .await;
    json::respond(result, debug)
}

async fn list_clients_records(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(
        list_collection_records(path.into_inner(), "clients", &params),
        debug,
    )
}

async fn list_products_records(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(
        list_collection_records(path.into_inner(), "products", &params),
        debug,
    )
}

async fn list_services_records(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(
        list_collection_records(path.into_inner(), "services", &params),
        debug,
    )
}

async fn list_domains_records(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(
        list_collection_records(path.into_inner(), "domains", &params),
        debug,
    )
}

async fn list_invoices_records(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(
        list_collection_records(path.into_inner(), "invoices", &params),
        debug,
    )
}

async fn list_server_accounts_records(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(
        list_collection_records(path.into_inner(), "server_accounts", &params),
        debug,
    )
}

async fn get_record(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(Uuid, String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SyncedRecord> = (|| {
        let (connection_id, collection_segment, remote_id) = path.into_inner();
        let collection = collection_from_segment(&collection_segment).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown collection \"{collection_segment}\""
            ))
        })?;
        let connection = find_connection(connection_id)?;
        let key: RecordKey = (
            connection_id,
            collection.to_string(),
            remote_id.clone(),
        );
        let _ = connection;
        records_store()
            .get(&key)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "Panel record \"{remote_id}\" in collection \"{collection}\" not found"
                ))
            })
    })();
    json::respond(result, debug)
}

async fn sync_now(ctx: web::Data<Arc<AppContext>>, path: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PanelSyncResult> = (|| async {
        let connection = find_connection(path.into_inner())?;
        require_enabled(&connection)?;
        Ok(sync_all_collections(&ctx, &connection).await)
    })()
    .await;
    json::respond(result, debug)
}

async fn sync_collection(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(Uuid, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PanelSyncResult> = (|| async {
        let (connection_id, collection_segment) = path.into_inner();
        let collection = collection_from_segment(&collection_segment).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown collection \"{collection_segment}\""
            ))
        })?;
        let connection = find_connection(connection_id)?;
        require_enabled(&connection)?;
        let outcome = sync_one_collection(&ctx, &connection, collection).await?;
        let mut counts = HashMap::new();
        counts.insert(
            collection.to_string(),
            outcome.inserted + outcome.updated + outcome.unchanged + outcome.conflicts + outcome.local_wins,
        );
        let now = Utc::now();
        if let Some(mut stored) = connections_store().get_mut(&connection.id) {
            stored.last_synced_at = Some(now);
            stored.updated_at = now;
        }
        ctx.hooks.emit(
            PANEL_SYNC_COMPLETED,
            serde_json::json!({
                "actor": "sync",
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "collection": collection,
                "inserted": outcome.inserted,
                "updated": outcome.updated,
                "unchanged": outcome.unchanged,
                "conflicts": outcome.conflicts,
            }),
        );
        Ok(PanelSyncResult {
            connection_id: connection.id,
            kind: connection.kind.as_str().to_string(),
            synced_at: now,
            counts,
            inserted: outcome.inserted,
            updated: outcome.updated,
            unchanged: outcome.unchanged,
            conflicts: outcome.conflicts,
            local_wins: outcome.local_wins,
            errors: HashMap::new(),
            success: true,
        })
    })()
    .await;
    json::respond(result, debug)
}

/// Two-way sync status: live-fetch every supported collection and
/// compare it with the stored records, reporting per collection what
/// changed locally vs remotely and how many conflicts exist. A
/// collection whose live fetch fails is reported with its error
/// instead of failing the whole endpoint.
async fn get_sync_status(ctx: web::Data<Arc<AppContext>>, path: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let connection = find_connection(path.into_inner())?;
        require_enabled(&connection)?;
        let adapter = panel_registry().build(&connection)?;
        let capabilities = adapter.capabilities();
        let mut collections = serde_json::Map::new();
        let mut total_conflicts: u64 = 0;
        let mut total_remote_changed: u64 = 0;
        let mut total_local_changed: u64 = 0;
        let mut total_only_remote: u64 = 0;
        let mut total_only_local: u64 = 0;
        for collection in COLLECTIONS {
            if !capabilities.supports(collection) {
                continue;
            }
            let stored = stored_records(connection.id, collection);
            let mut entry = serde_json::Map::new();
            entry.insert(
                "stored".to_string(),
                Value::from(stored.len() as u64),
            );
            match fetch_collection(&adapter, collection).await {
                Ok(remote) => {
                    let stored_by_id: HashMap<&str, &SyncedRecord> = stored
                        .iter()
                        .map(|record| (record.remote_id.as_str(), record))
                        .collect();
                    let mut unchanged: u64 = 0;
                    let mut remote_changed: u64 = 0;
                    let mut local_changed: u64 = 0;
                    let mut conflicts: u64 = 0;
                    let mut only_remote: Vec<String> = Vec::new();
                    for record in &remote {
                        match stored_by_id.get(record.remote_id.as_str()) {
                            None => {
                                only_remote.push(record.remote_id.clone());
                            }
                            Some(stored_record) => {
                                let hash = payload_hash(&record.payload);
                                if stored_record.local_changed && hash != stored_record.hash {
                                    conflicts += 1;
                                    local_changed += 1;
                                } else if stored_record.local_changed {
                                    local_changed += 1;
                                } else if hash != stored_record.hash {
                                    remote_changed += 1;
                                } else {
                                    unchanged += 1;
                                }
                            }
                        }
                    }
                    let remote_ids: std::collections::HashSet<&str> = remote
                        .iter()
                        .map(|record| record.remote_id.as_str())
                        .collect();
                    let only_local: Vec<String> = stored
                        .iter()
                        .filter(|record| !remote_ids.contains(record.remote_id.as_str()))
                        .map(|record| record.remote_id.clone())
                        .collect();
                    total_conflicts += conflicts;
                    total_remote_changed += remote_changed;
                    total_local_changed += local_changed;
                    total_only_remote += only_remote.len() as u64;
                    total_only_local += only_local.len() as u64;
                    entry.insert("remote".to_string(), Value::from(remote.len() as u64));
                    entry.insert("unchanged".to_string(), Value::from(unchanged));
                    entry.insert("remote_changed".to_string(), Value::from(remote_changed));
                    entry.insert("local_changed".to_string(), Value::from(local_changed));
                    entry.insert("conflicts".to_string(), Value::from(conflicts));
                    entry.insert(
                        "only_remote".to_string(),
                        Value::from(only_remote.len() as u64),
                    );
                    entry.insert(
                        "only_local".to_string(),
                        Value::from(only_local.len() as u64),
                    );
                    entry.insert(
                        "only_remote_ids".to_string(),
                        Value::Array(
                            only_remote
                                .into_iter()
                                .take(50)
                                .map(Value::String)
                                .collect(),
                        ),
                    );
                    entry.insert(
                        "only_local_ids".to_string(),
                        Value::Array(
                            only_local
                                .into_iter()
                                .take(50)
                                .map(Value::String)
                                .collect(),
                        ),
                    );
                }
                Err(error) => {
                    entry.insert("error".to_string(), Value::String(error.message));
                }
            }
            collections.insert(collection.to_string(), Value::Object(entry));
        }
        let state = sync_state_store().get(&connection.id);
        Ok(serde_json::json!({
            "connection_id": connection.id,
            "kind": connection.kind.as_str(),
            "last_synced_at": state
                .as_ref()
                .and_then(|state| state.last_synced_at)
                .map(|time| time.to_rfc3339()),
            "last_status": state
                .as_ref()
                .and_then(|state| state.last_status.clone()),
            "collections": collections,
            "totals": {
                "conflicts": total_conflicts,
                "remote_changed": total_remote_changed,
                "local_changed": total_local_changed,
                "only_remote": total_only_remote,
                "only_local": total_only_local,
            },
        }))
    })()
    .await;
    json::respond(result, debug)
}

async fn mark_local_changed(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(Uuid, String, String)>,
    body: web::Json<MarkLocalChangedRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SyncedRecord> = (|| {
        let (connection_id, collection_segment, remote_id) = path.into_inner();
        let collection = collection_from_segment(&collection_segment).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown collection \"{collection_segment}\""
            ))
        })?;
        let _ = find_connection(connection_id)?;
        let key: RecordKey = (
            connection_id,
            collection.to_string(),
            remote_id.clone(),
        );
        let mut stored = records_store().get_mut(&key).ok_or_else(|| {
            UnifiedError::not_found(format!(
                "Panel record \"{remote_id}\" in collection \"{collection}\" not found"
            ))
        })?;
        stored.local_changed = true;
        stored.prefer_local = body.prefer_local;
        stored.direction = SyncDirection::LocalToRemote.as_str().to_string();
        Ok(stored.clone())
    })();
    json::respond(result, debug)
}

async fn resolve_conflict(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(Uuid, String, String)>,
    body: web::Json<ResolveConflictRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let (connection_id, collection_segment, remote_id) = path.into_inner();
        let collection = collection_from_segment(&collection_segment).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown collection \"{collection_segment}\""
            ))
        })?;
        let connection = find_connection(connection_id)?;
        require_enabled(&connection)?;
        let resolution = body.resolution.trim();
        if !matches!(resolution, "remote_wins" | "local_wins") {
            return Err(UnifiedError::bad_request(
                "resolution must be \"remote_wins\" or \"local_wins\"",
            ));
        }
        let key: RecordKey = (
            connection_id,
            collection.to_string(),
            remote_id.clone(),
        );
        match resolution {
            "local_wins" => {
                let mut stored = records_store().get_mut(&key).ok_or_else(|| {
                    UnifiedError::not_found(format!(
                        "Panel record \"{remote_id}\" in collection \"{collection}\" not found"
                    ))
                })?;
                stored.prefer_local = true;
                stored.local_changed = false;
                stored.conflict = false;
                stored.direction = SyncDirection::LocalToRemote.as_str().to_string();
                stored.last_synced_at = Utc::now();
                let record = stored.clone();
                drop(stored);
                ctx.hooks.emit(
                    PANEL_SYNC_CONFLICT,
                    serde_json::json!({
                        "actor": "api",
                        "connection_id": connection.id,
                        "kind": connection.kind.as_str(),
                        "resource": format!("panel-{collection}:{remote_id}"),
                        "collection": collection,
                        "remote_id": remote_id,
                        "resolution": "local_wins",
                    }),
                );
                Ok(serde_json::json!({ "record": record, "resolution": "local_wins" }))
            }
            _ => {
                // Remote wins: re-fetch the record live and apply it,
                // clearing the local flags. When the remote side no
                // longer has the record, the stored record is removed
                // and the deletion is reported.
                let adapter = panel_registry().build(&connection)?;
                let remote = fetch_collection(&adapter, collection).await?;
                let found = remote
                    .iter()
                    .find(|record| record.remote_id == remote_id)
                    .cloned();
                match found {
                    Some(record) => {
                        let hash = payload_hash(&record.payload);
                        let mut stored = records_store().get_mut(&key).ok_or_else(|| {
                            UnifiedError::not_found(format!(
                                "Panel record \"{remote_id}\" in collection \"{collection}\" not found"
                            ))
                        })?;
                        stored.hash = hash;
                        stored.name = record.name.clone();
                        stored.status = record.status.clone();
                        stored.payload = record.payload.clone();
                        stored.local_changed = false;
                        stored.prefer_local = false;
                        stored.conflict = false;
                        stored.direction =
                            SyncDirection::RemoteToLocal.as_str().to_string();
                        stored.last_synced_at = Utc::now();
                        let view = stored.clone();
                        drop(stored);
                        ctx.hooks.emit(
                            PANEL_SYNC_CONFLICT,
                            serde_json::json!({
                                "actor": "api",
                                "connection_id": connection.id,
                                "kind": connection.kind.as_str(),
                                "resource": format!("panel-{collection}:{remote_id}"),
                                "collection": collection,
                                "remote_id": remote_id,
                                "resolution": "remote_wins",
                            }),
                        );
                        Ok(serde_json::json!({ "record": view, "resolution": "remote_wins" }))
                    }
                    None => {
                        records_store().remove(&key);
                        Ok(serde_json::json!({
                            "resolution": "remote_wins",
                            "remote_id": remote_id,
                            "deleted_locally": true,
                            "detail": "Remote record no longer exists; the stored record was removed",
                        }))
                    }
                }
            }
        }
    })()
    .await;
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct PanelsModule;

impl Module for PanelsModule {
    fn id(&self) -> &'static str {
        "panels"
    }

    fn name(&self) -> &'static str {
        "Panel Integrations"
    }

    fn description(&self) -> &'static str {
        "Hosting and billing panel connectors: WHMCS, cPanel/WHM, DirectAdmin, Plesk, CyberPanel, and a configuration-driven generic panel, with a hookable adapter registry, masked credentials, in-memory import with two-way sync and conflict reporting."
    }

    fn register_hooks(&self, _bus: &HookBus) {
        // Imported records are stored data, not a cache, so they are
        // deliberately NOT cleared on `app.reload`: a reload must not
        // discard synced records. Hook wiring for this module is
        // emission only (see `panel.connected`, `panel.client.synced`,
        // `panel.service.synced`, `panel.sync.completed`,
        // `panel.sync.failed`, and `panel.sync.conflict`); the audit
        // module subscribes to those events for its trail.
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/panels")
                .route("", web::get().to(list_kinds))
                .route("/connections", web::get().to(list_connections))
                .route("/connections", web::post().to(create_connection))
                .route("/connections/{id}", web::get().to(get_connection))
                .route("/connections/{id}", web::patch().to(update_connection))
                .route("/connections/{id}", web::delete().to(delete_connection))
                .route("/connections/{id}/test", web::post().to(test_connection))
                .route("/connections/{id}/clients", web::get().to(list_clients_records))
                .route("/connections/{id}/products", web::get().to(list_products_records))
                .route("/connections/{id}/services", web::get().to(list_services_records))
                .route("/connections/{id}/domains", web::get().to(list_domains_records))
                .route("/connections/{id}/invoices", web::get().to(list_invoices_records))
                .route(
                    "/connections/{id}/server-accounts",
                    web::get().to(list_server_accounts_records),
                )
                .route(
                    "/connections/{id}/records/{collection}/{remote_id}",
                    web::get().to(get_record),
                )
                .route("/connections/{id}/sync", web::post().to(sync_now))
                .route("/connections/{id}/sync-now", web::post().to(sync_now))
                .route("/connections/{id}/sync/{collection}", web::post().to(sync_collection))
                .route("/connections/{id}/sync-status", web::get().to(get_sync_status))
                .route(
                    "/connections/{id}/records/{collection}/{remote_id}/local-change",
                    web::post().to(mark_local_changed),
                )
                .route(
                    "/connections/{id}/records/{collection}/{remote_id}/resolve-conflict",
                    web::post().to(resolve_conflict),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/panels",
                owner: "panels",
                description: "List panel kinds and their capabilities.",
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/panels/connections",
                owner: "panels",
                description: "List panel connections (credentials masked).",
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/panels/connections",
                owner: "panels",
                description: "Register a panel connection (credentials write-only).",
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/panels/connections/{id}",
                owner: "panels",
                description: "Get one panel connection (credentials masked).",
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/panels/connections/{id}",
                owner: "panels",
                description: "Update a panel connection (credentials merge, values never returned).",
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/panels/connections/{id}",
                owner: "panels",
                description: "Delete a panel connection and its imported records.",
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/panels/connections/{id}/test",
                owner: "panels",
                description: "Test a panel connection against the real panel API.",
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/panels/connections/{id}/clients",
                owner: "panels",
                description: "List imported clients for a connection (paginated, filterable).",
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/panels/connections/{id}/products",
                owner: "panels",
                description: "List imported products for a connection (paginated, filterable).",
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/panels/connections/{id}/services",
                owner: "panels",
                description: "List imported services for a connection (paginated, filterable).",
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/panels/connections/{id}/domains",
                owner: "panels",
                description: "List imported domains for a connection (paginated, filterable).",
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/panels/connections/{id}/invoices",
                owner: "panels",
                description: "List imported invoices for a connection (paginated, filterable).",
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/panels/connections/{id}/server-accounts",
                owner: "panels",
                description: "List imported server accounts for a connection (paginated, filterable).",
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/panels/connections/{id}/records/{collection}/{remote_id}",
                owner: "panels",
                description: "Get one imported record with its sync state.",
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/panels/connections/{id}/sync",
                owner: "panels",
                description: "Sync every supported collection for a connection now.",
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/panels/connections/{id}/sync-now",
                owner: "panels",
                description: "Manual sync-now: sync every supported collection for a connection.",
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/panels/connections/{id}/sync/{collection}",
                owner: "panels",
                description: "Sync one collection for a connection now.",
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/panels/connections/{id}/sync-status",
                owner: "panels",
                description: "Two-way sync status: local vs remote changes and conflicts per collection.",
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/panels/connections/{id}/records/{collection}/{remote_id}/local-change",
                owner: "panels",
                description: "Flag a stored record as locally changed (optionally preferring the local version).",
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/panels/connections/{id}/records/{collection}/{remote_id}/resolve-conflict",
                owner: "panels",
                description: "Resolve a sync conflict with remote_wins or local_wins.",
            },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    // Force the built-in adapters to register before the module is used.
    let _ = panel_registry();
    registry.register(Arc::new(PanelsModule));
}
