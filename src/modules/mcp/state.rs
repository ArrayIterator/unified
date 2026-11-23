//! mcp - state.

use unified_core::context::AppContext;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::protocol::*;
use super::catalog::*;

/// One scoped MCP server: the sellable unit. A customer buys "MCP for
/// my site" and gets one of these, mounted at
/// `/api/v1/mcp/servers/{id or slug}/mcp`.
#[derive(Debug, Clone, Serialize)]
pub struct McpServer {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    /// Owner (user id) the server belongs to. API keys whose
    /// `owner_id` is set may only use servers owned by that user.
    pub owner_id: Uuid,
    pub company_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    /// Product the server was bought as (Product Management id).
    /// `None` means the server was created without a product link
    /// (platform-owned, not sold through the store).
    pub product_id: Option<String>,
    /// `"active"` or `"disabled"`. Disabled servers answer 403.
    pub status: String,
    /// Public servers are listed in the public registry and reachable
    /// under the public API prefix; private servers answer only to
    /// keys scoped to them.
    pub is_public: bool,
    /// Extra per-server tool allowlist. Empty means no tool is
    /// allowed (fail closed). A server created without an explicit
    /// list gets the full built-in list stored explicitly instead.
    /// A tool must pass both this list and the key's scopes.
    pub allowed_tools: Vec<String>,
    /// Per-server request ceiling (requests per minute). The key's
    /// own tier still applies on top of this; the lower of the two
    /// wins for any single call.
    pub rate_limit_per_minute: u32,
    /// Optional PostgreSQL role the read-only queries run as
    /// (`SET LOCAL ROLE`). Set by an admin; validated as a bare
    /// identifier. `None` means the pool's own role, still inside a
    /// read-only transaction.
    pub read_only_role: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl McpServer {
    /// Public view of a server: everything except internal notes.
    /// The view never contains a key, a secret, or a database role
    /// password (the role name is not a secret, but it is still only
    /// shown on the admin endpoints, never on the public one).
    pub(crate) fn public_view(&self) -> Value {
        json!({
            "id": self.id,
            "slug": self.slug,
            "name": self.name,
            "owner_id": self.owner_id,
            "company_id": self.company_id,
            "site_id": self.site_id,
            "product_id": self.product_id,
            "status": self.status,
            "is_public": self.is_public,
            "allowed_tools": self.allowed_tools,
            "rate_limit_per_minute": self.rate_limit_per_minute,
            "endpoint": format!("/api/v1/mcp/servers/{}/mcp", self.slug),
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        })
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateServerRequest {
    pub(crate) name: String,
    pub(crate) slug: String,
    pub(crate) owner_id: Uuid,
    #[serde(default)]
    pub(crate) company_id: Option<Uuid>,
    #[serde(default)]
    pub(crate) site_id: Option<Uuid>,
    #[serde(default)]
    pub(crate) product_id: Option<String>,
    #[serde(default)]
    pub(crate) is_public: bool,
    #[serde(default)]
    pub(crate) allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) rate_limit_per_minute: Option<u32>,
    #[serde(default)]
    pub(crate) read_only_role: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateServerRequest {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) status: Option<String>,
    #[serde(default)]
    pub(crate) is_public: Option<bool>,
    #[serde(default)]
    pub(crate) allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) rate_limit_per_minute: Option<u32>,
    #[serde(default)]
    pub(crate) read_only_role: Option<Option<String>>,
    #[serde(default)]
    pub(crate) product_id: Option<Option<String>>,
}

/// Module configuration, as reported by `GET /api/v1/mcp/config`.
/// The read-only allowlist and denylist are part of the config
/// response on purpose: an operator can audit exactly what the MCP
/// server may read without reading source code.
#[derive(Debug, Clone, Serialize)]
pub struct McpConfigView {
    pub read_only: bool,
    pub protocol_version: String,
    pub read_only_role: Option<String>,
    pub default_rate_limit_per_minute: u32,
    pub allowlisted_tables: Vec<Value>,
    pub denylisted_columns: Vec<String>,
    pub context_sources: Vec<Value>,
    pub writes_supported: bool,
    pub writes_note: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateConfigRequest {
    #[serde(default)]
    pub(crate) read_only_role: Option<Option<String>>,
    #[serde(default)]
    pub(crate) default_rate_limit_per_minute: Option<u32>,
}

#[derive(Debug, Clone)]
pub(crate) struct McpConfig {
    pub(crate) read_only_role: Option<String>,
    pub(crate) default_rate_limit_per_minute: u32,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            read_only_role: None,
            default_rate_limit_per_minute: 60,
        }
    }
}

/// One per-key per-minute rate bucket.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RateBucket {
    pub(crate) minute: u64,
    pub(crate) count: u32,
}

#[derive(Clone)]
pub struct McpState {
    pub servers: Arc<DashMap<Uuid, McpServer>>,
    /// Slug -> server id, for mounting by slug.
    pub servers_by_slug: Arc<DashMap<String, Uuid>>,
    pub rate_buckets: Arc<DashMap<String, RateBucket>>,
    pub config: Arc<RwLock<McpConfig>>,
}

impl Default for McpState {
    fn default() -> Self {
        Self {
            servers: Arc::new(DashMap::new()),
            servers_by_slug: Arc::new(DashMap::new()),
            rate_buckets: Arc::new(DashMap::new()),
            config: Arc::new(RwLock::new(McpConfig::default())),
        }
    }
}

impl McpState {
    pub(crate) fn config_view(&self, ctx: &AppContext) -> McpConfigView {
        let config = self
            .config
            .read()
            .expect("mcp config lock poisoned")
            .clone();
        McpConfigView {
            read_only: true,
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
            read_only_role: config.read_only_role,
            default_rate_limit_per_minute: config.default_rate_limit_per_minute,
            allowlisted_tables: READ_ONLY_TABLES
                .iter()
                .map(|table| {
                    json!({
                        "table": table.name,
                        "columns": table.columns,
                        "search_column": table.search_column,
                        "description": table.description,
                    })
                })
                .collect(),
            denylisted_columns: DENYLISTED_COLUMNS
                .iter()
                .map(|entry| entry.to_string())
                .collect(),
            context_sources: CONTEXT_SOURCES
                .iter()
                .map(|source| {
                    json!({
                        "source": source.name,
                        "description": source.description,
                        "owner_module": source.owner_module,
                        "hook_event": format!("{}{}", mcp_events::CONTEXT_PREFIX, source.name),
                        "answered": ctx.hooks.has(&format!(
                            "{}{}",
                            mcp_events::CONTEXT_PREFIX,
                            source.name
                        )),
                        "db_fallback_table": source.db_table,
                    })
                })
                .collect(),
            writes_supported: false,
            writes_note: "MVP MCP is read-only. Write-capable tools are a future, admin-gated feature; they must go through the owning modules' own write paths, never through direct MCP SQL.".to_string(),
        }
    }
}
