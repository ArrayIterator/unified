// MCP server module — Model Context Protocol access to the site's own context.
//
// The module mounts one scoped MCP server per site (a "server record":
// owner, company/site scope, product/entitlement reference) and speaks
// the MCP wire protocol (JSON-RPC 2.0) at
// `POST /api/v1/mcp/servers/{server_id_or_slug}/mcp`. Every other
// endpoint in this module is an ordinary admin REST endpoint and uses
// the standard envelope (`{"data": ...}` on success,
// `{"error": {"message", "code", "metadata"}}` on error, `trace` in
// debug mode only). The MCP endpoint itself must answer JSON-RPC 2.0
// (`{"jsonrpc": "2.0", "id": ..., "result": ...}` or `"error"`), because
// that is the wire shape MCP clients speak; the read-only guarantee,
// auth, and scoping below apply to it exactly as they do to the REST
// surface.
//
// READ-ONLY, MVP (hard rule). The MCP server never writes. There is no
// write-capable tool in this module, and no MCP client input ever
// becomes SQL: clients only ever supply *values* (bound parameters)
// and the names of allowlisted tools, resources, and prompts. Direct
// database reads go through `read_only_query`, which:
//
//   1. resolves the requested table against `READ_ONLY_TABLES`, an
//      explicit allowlist of tables and columns (visible at
//      `GET /api/v1/mcp/config` and in every `db.query` answer);
//   2. rejects any requested column that appears in
//      `DENYLISTED_COLUMNS` (credential, token, secret, password, and
//      key columns) before any SQL is built, and scrubs those keys out
//      of every returned row as defence in depth;
//   3. runs the query inside one Diesel transaction whose first
//      statement is `SET TRANSACTION READ ONLY`, so PostgreSQL itself
//      refuses any write, and (when an admin configured one)
//      `SET LOCAL ROLE <read-only role>` first;
//   4. binds every client-supplied value as a Diesel parameter
//      (`$1`, `$2`, ...); table and column identifiers come only from
//      the allowlist constants, never from client input.
//
// Tables that hold secrets are absent from the allowlist entirely:
// `users` (`password_hash`, `totp_secret`), `licenses` and
// `store_entitlements` (`license_key`), and `vcs_provider_settings`
// (provider credentials). Context that lives in other modules
// (content, products, orders, tickets, knowledgebase, settings, email
// templates, newsletter, site builder) is assembled through the hook
// bus, never through raw table dumps: the MCP module emits
// `mcp.context.<source>` with `{"server_id", "page", "per_page",
// "search", "filters"}` and the owning module answers with
// `{"items": [...], "total": n}`. The one exception is the shared
// `site` source: the AI module owns the one context assembler
// (`assemble_shared_context` in ai.rs), and the MCP module calls it
// directly for `site` (see `fetch_context`) instead of building a
// second assembler or asking through a `mcp.context.site` hook.
// No source module answers those events yet (verified while writing
// this module), so until a source module registers a handler every
// context answer reports `"available": false` with a gap note
// instead of inventing data.
// The one context event that already exists elsewhere is
// `knowledgebase.search`, answered by the knowledgebase module; the
// `knowledgebase.search` tool uses it when it is registered and
// reports the same gap otherwise.
//
// AUTH. MCP clients authenticate with API keys through the API Keys
// module (`crate::modules::apikeys::authenticate_key`, the same
// verification the auth middleware uses), presented as
// `Authorization: Bearer <key>` or `X-Api-Key`. A key may only call
// the tools it is scoped to: `tools/call` requires the scope
// `mcp:<tool-name>` (or `mcp:*` / `*`), and the read-only protocol
// methods require `mcp:read`. The server record adds a second
// allowlist (`allowed_tools`); a tool must pass both. A key whose
// `owner_id` is set may only use servers owned by that user. Usage is
// recorded with `apikeys::note_used`. Rate limits reuse the key's own
// tier (`requests_per_minute` from the API Keys module); the module
// keeps only a per-key per-minute counter, it does not invent a new
// tier system.
//
// PUBLIC / PRIVATE. MCP routes are declared in `route_info`, so the
// API Manager collects them into its built-in endpoint registry at
// startup (`ApiManagerState::collect_from_catalog`) and publishes them
// under the versioned public prefix (`/v1/mcp/...`) exactly like every
// other module's routes. Whether a given server is reachable publicly
// is governed by the server record's `is_public` flag together with
// API Management's allowed-public-access controls and the per-key
// scopes and rate limits above; this module rebuilds none of that.
// A private server (`is_public: false`) still answers on its mounted
// path, but only to keys scoped to it.
//
// SELLABLE. MCP access is a product: a customer buys "MCP for my
// site" and gets a scoped server. The server record carries
// `product_id`, and creation (and every later entitlement re-check)
// asks Product Management through the `product.entitlement.check`
// hook event (`{"product_id", "user_id"}` in, `{"entitled": bool}`
// out — answered by the products module). The check is fail-closed:
// no handler answering, or an explicit `{"entitled": false}`, refuses
// the server. The module never reads product or order tables itself
// for this; the hook is the only source of truth.
//
// Hooks emitted: `mcp.server.created`, `mcp.server.updated`,
// `mcp.server.deleted`, `mcp.tool.called`, `mcp.client.authenticated`,
// `mcp.context.served`. Hooks honoured: `mcp.context.<source>` (asked
// of the source modules), `mcp.tools.extend` (other modules may
// contribute extra read-only tools, which are listed and dispatched
// through `mcp.tool.call_requested`), `product.entitlement.check`
// (asked of Product Management). This module answers
// `mcp.server.exists` with `{"exists": bool}`.
//
// OUT OF SCOPE for this round (recorded, not built): any write-capable
// MCP tool. Writes are a future, admin-gated feature and must go
// through the owning modules' own write paths with their own
// authorisation, never through direct MCP SQL.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::db::DbPool;
use crate::modules::ai::{assemble_shared_context, SiteContextInput};
use crate::modules::apikeys::{authenticate_key, key_has_scope, note_used, AuthenticatedKey};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Text};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Constants.
// ---------------------------------------------------------------------------

/// MCP protocol version this server speaks (2024-11-05).
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
/// JSON-RPC error code: method not found.
const JSONRPC_METHOD_NOT_FOUND: i64 = -32_601;
/// JSON-RPC error code: invalid params.
const JSONRPC_INVALID_PARAMS: i64 = -32_602;
/// JSON-RPC error code: internal / source error.
const JSONRPC_SERVER_ERROR: i64 = -32_000;
/// JSON-RPC error code: a context source (or tool provider) did not answer.
const JSONRPC_SOURCE_UNAVAILABLE: i64 = -32_002;

/// Well-known events emitted by this module.
pub mod mcp_events {
    pub const SERVER_CREATED: &str = "mcp.server.created";
    pub const SERVER_UPDATED: &str = "mcp.server.updated";
    pub const SERVER_DELETED: &str = "mcp.server.deleted";
    pub const TOOL_CALLED: &str = "mcp.tool.called";
    pub const CLIENT_AUTHENTICATED: &str = "mcp.client.authenticated";
    pub const CONTEXT_SERVED: &str = "mcp.context.served";
    /// Emitted by tools/list so other modules can contribute tools.
    pub const TOOLS_EXTEND: &str = "mcp.tools.extend";
    /// Emitted when a hook-contributed tool is called; the owning
    /// module answers with `{"result": ...}`.
    pub const TOOL_CALL_REQUESTED: &str = "mcp.tool.call_requested";
    /// Asked of the source modules, one event per context source.
    pub const CONTEXT_PREFIX: &str = "mcp.context.";
}

/// Column-name fragments that are never exposed, in any table, for any
/// caller. Checked against every column a query would return, and
/// scrubbed out of every returned row as defence in depth. A column
/// is denied when its lower-cased name contains one of these
/// fragments as a whole word or suffix: `password_hash` matches
/// `password`, `api_key` matches `key`, and so on.
pub const DENYLISTED_COLUMNS: &[&str] = &[
    "password",
    "secret",
    "token",
    "credential",
    "private_key",
    "api_key",
    "key_hash",
    "license_key",
    "totp",
    "salt",
];

/// One table the MCP `db.query` tool (and only that tool) may read,
/// with the exact columns it may return. The list is deliberately
/// small: tables that hold secrets (`users`, `licenses`,
/// `store_entitlements`, `vcs_provider_settings`) are absent
/// entirely, and `store_orders` exposes no `user_id` column.
pub struct ReadOnlyTable {
    /// Table name as it appears in `src/db/schema.rs`.
    pub name: &'static str,
    /// Columns the tool may select, in output order.
    pub columns: &'static [&'static str],
    /// Column the free-text `search` argument matches against.
    pub search_column: &'static str,
    /// What the table holds, for the config endpoint.
    pub description: &'static str,
}

pub const READ_ONLY_TABLES: &[ReadOnlyTable] = &[
    ReadOnlyTable {
        name: "companies",
        columns: &[
            "id",
            "name",
            "slug",
            "is_active",
            "created_at",
            "updated_at",
        ],
        search_column: "name",
        description: "Companies (tenants) the site belongs to.",
    },
    ReadOnlyTable {
        name: "company_sites",
        columns: &[
            "id",
            "company_id",
            "name",
            "domain",
            "is_primary",
            "is_active",
            "created_at",
            "updated_at",
        ],
        search_column: "name",
        description: "Sites belonging to a company.",
    },
    ReadOnlyTable {
        name: "store_products",
        columns: &[
            "id",
            "name",
            "description",
            "price",
            "currency",
            "kind",
            "status",
            "slug",
            "created_at",
            "updated_at",
        ],
        search_column: "name",
        description: "Sellable products in the store catalog.",
    },
    ReadOnlyTable {
        name: "store_orders",
        columns: &[
            "id",
            "subtotal",
            "discount_total",
            "total",
            "currency",
            "coupon_code",
            "company_id",
            "status",
            "provider",
            "created_at",
            "updated_at",
        ],
        search_column: "status",
        description: "Store orders (no customer user id is exposed).",
    },
    ReadOnlyTable {
        name: "store_order_items",
        columns: &[
            "id",
            "order_id",
            "product_id",
            "name",
            "unit_price",
            "quantity",
            "line_total",
            "tier_name",
            "created_at",
            "updated_at",
        ],
        search_column: "name",
        description: "Line items of store orders.",
    },
];

/// Context sources assembled through the hook bus. Each source maps
/// to one `mcp.context.<source>` event and, where one exists, to the
/// owning module's read/list route. `db_table` names the
/// `READ_ONLY_TABLES` entry the source falls back to when the owning
/// module does not answer the hook; `None` means there is no database
/// table for the source at all (the module keeps its data in memory)
/// and the only honest answer is the gap note.
pub struct ContextSource {
    pub name: &'static str,
    pub description: &'static str,
    pub owner_module: &'static str,
    pub db_table: Option<&'static str>,
}

pub const CONTEXT_SOURCES: &[ContextSource] = &[
    ContextSource {
        name: "site",
        description: "Shared site context (site facts plus knowledgebase grounding) from the AI module's single context source.",
        owner_module: "ai",
        db_table: None,
    },
    ContextSource {
        name: "content",
        description: "Posts, categories, and pages from the Content module.",
        owner_module: "content",
        db_table: None,
    },
    ContextSource {
        name: "products",
        description: "Sellable products from Product Management and the store catalog.",
        owner_module: "products",
        db_table: Some("store_products"),
    },
    ContextSource {
        name: "orders",
        description: "Store orders and their line items.",
        owner_module: "store",
        db_table: Some("store_orders"),
    },
    ContextSource {
        name: "tickets",
        description: "Support tickets and their status.",
        owner_module: "tickets",
        db_table: None,
    },
    ContextSource {
        name: "knowledgebase",
        description: "Knowledgebase articles, ranked by the knowledgebase search.",
        owner_module: "knowledgebase",
        db_table: None,
    },
    ContextSource {
        name: "settings",
        description: "Public site settings (never credentials or secrets).",
        owner_module: "settings",
        db_table: None,
    },
    ContextSource {
        name: "email_templates",
        description: "Email templates (subject and body, no sending).",
        owner_module: "email_templates",
        db_table: None,
    },
    ContextSource {
        name: "newsletter",
        description: "Newsletter campaigns and their content.",
        owner_module: "newsletter",
        db_table: None,
    },
    ContextSource {
        name: "site_builder",
        description: "Website-builder pages, blocks, and sections.",
        owner_module: "site_builder",
        db_table: None,
    },
];

/// The tools this module ships. Every tool is read-only; there is no
/// write-capable tool in this module by design.
pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON Schema (as a JSON value) for the tool's arguments.
    pub input_schema: Value,
}

/// Small helper: a JSON Schema object with the given properties and
/// required list. Keeps the tool table readable.
fn tool_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn paging_properties() -> Value {
    json!({
        "page": { "type": "integer", "minimum": 1, "default": 1 },
        "per_page": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
        "search": { "type": "string" },
    })
}

pub fn builtin_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "site.search",
            description: "Search every context source at once (shared site context, content, products, orders, tickets, knowledgebase, settings, email templates, newsletter, site-builder pages). Read-only.",
            input_schema: tool_schema(
                json!({
                    "query": { "type": "string" },
                    "sources": { "type": "array", "items": { "type": "string" } },
                    "page": { "type": "integer", "minimum": 1, "default": 1 },
                    "per_page": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
                }),
                &["query"],
            ),
        },
        ToolDefinition {
            name: "site.context",
            description: "Shared site context (site facts plus knowledgebase grounding) from the AI module's single context source. Read-only.",
            input_schema: tool_schema(
                json!({
                    "query": { "type": "string" },
                }),
                &[],
            ),
        },
        ToolDefinition {
            name: "content.list",
            description: "List content posts and pages. Read-only; assembled from the Content module through the mcp.context.content hook.",
            input_schema: tool_schema(paging_properties(), &[]),
        },
        ToolDefinition {
            name: "content.get",
            description: "Get one content post or page by id. Read-only.",
            input_schema: tool_schema(
                json!({ "id": { "type": "string" } }),
                &["id"],
            ),
        },
        ToolDefinition {
            name: "products.list",
            description: "List sellable products. Read-only; assembled from Product Management through the mcp.context.products hook, with a read-only store_products query as fallback.",
            input_schema: tool_schema(paging_properties(), &[]),
        },
        ToolDefinition {
            name: "products.get",
            description: "Get one product by id. Read-only.",
            input_schema: tool_schema(
                json!({ "id": { "type": "string" } }),
                &["id"],
            ),
        },
        ToolDefinition {
            name: "orders.list",
            description: "List store orders. Read-only; assembled from the Store module through the mcp.context.orders hook, with a read-only store_orders query as fallback.",
            input_schema: tool_schema(paging_properties(), &[]),
        },
        ToolDefinition {
            name: "tickets.list",
            description: "List support tickets. Read-only; assembled from the Tickets module through the mcp.context.tickets hook.",
            input_schema: tool_schema(paging_properties(), &[]),
        },
        ToolDefinition {
            name: "knowledgebase.search",
            description: "Ranked full-text search over knowledgebase articles, answered by the knowledgebase.search hook. Read-only.",
            input_schema: tool_schema(
                json!({
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 5 },
                }),
                &["query"],
            ),
        },
        ToolDefinition {
            name: "email_templates.list",
            description: "List email templates (subject and body). Read-only; sending is never exposed.",
            input_schema: tool_schema(paging_properties(), &[]),
        },
        ToolDefinition {
            name: "newsletter.list",
            description: "List newsletter campaigns and their content. Read-only.",
            input_schema: tool_schema(paging_properties(), &[]),
        },
        ToolDefinition {
            name: "site_builder.pages.list",
            description: "List website-builder pages. Read-only; assembled from the Site Builder module through the mcp.context.site_builder hook.",
            input_schema: tool_schema(paging_properties(), &[]),
        },
        ToolDefinition {
            name: "db.query",
            description: "Read-only query over the explicit allowlist of tables and columns (see the module config endpoint). Only bound parameters are accepted; no raw SQL, no writes, no DDL, no credential columns.",
            input_schema: tool_schema(
                json!({
                    "table": { "type": "string" },
                    "columns": { "type": "array", "items": { "type": "string" } },
                    "search": { "type": "string" },
                    "page": { "type": "integer", "minimum": 1, "default": 1 },
                    "per_page": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
                }),
                &["table"],
            ),
        },
    ]
}

/// Resources this module exposes. A resource URI is resolved through
/// the same hook context fetch as the matching tool.
pub const RESOURCES: &[(&str, &str, &str)] = &[
    ("unified://site/context", "Shared site context", "site"),
    ("unified://content/posts", "Content posts and pages", "content"),
    ("unified://products", "Sellable products", "products"),
    ("unified://orders", "Store orders", "orders"),
    ("unified://tickets", "Support tickets", "tickets"),
    ("unified://knowledgebase/articles", "Knowledgebase articles", "knowledgebase"),
    ("unified://settings/public", "Public site settings", "settings"),
    ("unified://email-templates", "Email templates", "email_templates"),
    ("unified://newsletter/campaigns", "Newsletter campaigns", "newsletter"),
    ("unified://site-builder/pages", "Website-builder pages", "site_builder"),
];

/// Prompts this module exposes. Arguments are substituted into the
/// prompt text only; a prompt never executes anything.
pub struct PromptDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub arguments: &'static [(&'static str, &'static str, bool)],
    pub template: &'static str,
}

pub const PROMPTS: &[PromptDefinition] = &[
    PromptDefinition {
        name: "site-brief",
        description: "Brief an assistant on this site: what it sells, what it publishes, and where its help content lives.",
        arguments: &[("focus", "What to emphasise (for example: products, support, content)", false)],
        template: "You are helping with the site served by this MCP server. Use only the context the tools return: products from products.list, published content from content.list, and help articles from knowledgebase.search. Focus: {{focus}}. Never invent prices, articles, or tickets that the tools did not return.",
    },
    PromptDefinition {
        name: "support-triage",
        description: "Triage open support tickets: summarise what is open and what the knowledgebase already covers.",
        arguments: &[("ticket_id", "One ticket to triage in depth (optional)", false)],
        template: "Triage the support tickets from tickets.list (or the single ticket {{ticket_id}} when one is given). For each ticket, check knowledgebase.search for an article that answers it and say so explicitly; do not claim an article exists when the search returned none.",
    },
    PromptDefinition {
        name: "product-announcement",
        description: "Draft a product announcement from the product catalog and the latest newsletter content.",
        arguments: &[("product_id", "The product to announce", true)],
        template: "Draft a short announcement for product {{product_id}} using only products.get and newsletter.list. Keep every price, name, and feature exactly as the tools returned it; if a detail is missing from the tools, leave it out instead of inventing it.",
    },
    PromptDefinition {
        name: "knowledgebase-answer",
        description: "Answer a question grounded in the knowledgebase, with citations to the articles used.",
        arguments: &[("question", "The question to answer", true)],
        template: "Answer this question using only knowledgebase.search: {{question}}. Cite the article id and title for every claim. When no article covers the question, say so plainly instead of answering from general knowledge.",
    },
];

// ---------------------------------------------------------------------------
// Types: servers, configuration, state.
// ---------------------------------------------------------------------------

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
    /// Extra per-server tool allowlist. Empty means "every built-in
    /// tool the key is scoped to". A tool must pass both this list
    /// and the key's scopes.
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
    fn public_view(&self) -> Value {
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
            "public_endpoint": format!("/v1/mcp/servers/{}/mcp", self.slug),
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        })
    }
}

#[derive(Debug, Deserialize)]
struct CreateServerRequest {
    name: String,
    slug: String,
    owner_id: Uuid,
    #[serde(default)]
    company_id: Option<Uuid>,
    #[serde(default)]
    site_id: Option<Uuid>,
    #[serde(default)]
    product_id: Option<String>,
    #[serde(default)]
    is_public: bool,
    #[serde(default)]
    allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    rate_limit_per_minute: Option<u32>,
    #[serde(default)]
    read_only_role: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateServerRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    is_public: Option<bool>,
    #[serde(default)]
    allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    rate_limit_per_minute: Option<u32>,
    #[serde(default)]
    read_only_role: Option<Option<String>>,
    #[serde(default)]
    product_id: Option<Option<String>>,
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
struct UpdateConfigRequest {
    #[serde(default)]
    read_only_role: Option<Option<String>>,
    #[serde(default)]
    default_rate_limit_per_minute: Option<u32>,
}

#[derive(Debug, Clone)]
struct McpConfig {
    read_only_role: Option<String>,
    default_rate_limit_per_minute: u32,
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
struct RateBucket {
    minute: u64,
    count: u32,
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
    fn config_view(&self, ctx: &AppContext) -> McpConfigView {
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

// ---------------------------------------------------------------------------
// Validation helpers.
// ---------------------------------------------------------------------------

/// True when `column` is denied: its lower-cased name contains a
/// denylisted fragment. The check is deliberately broad — a column
/// called `api_key_hash` or `password_hash` is denied by the `key`
/// and `password` fragments even though the full name is not listed.
pub fn column_is_denied(column: &str) -> bool {
    let lower = column.to_lowercase();
    DENYLISTED_COLUMNS.iter().any(|fragment| {
        lower == *fragment
            || lower.ends_with(&format!("_{fragment}"))
            || lower.starts_with(&format!("{fragment}_"))
            || lower.contains(&format!("_{fragment}_"))
    })
}

fn find_table(name: &str) -> Option<&'static ReadOnlyTable> {
    READ_ONLY_TABLES.iter().find(|table| table.name == name)
}

fn find_source(name: &str) -> Option<&'static ContextSource> {
    CONTEXT_SOURCES.iter().find(|source| source.name == name)
}

fn tool_is_known(name: &str) -> bool {
    builtin_tools().iter().any(|tool| tool.name == name)
}

/// Validate a server slug: 3..=64 chars, lower-case letters, digits,
/// and hyphens only, not starting or ending with a hyphen.
fn validate_slug(slug: &str) -> UnifiedResult<String> {
    let slug = slug.trim().to_lowercase();
    if slug.chars().count() < 3 || slug.chars().count() > 64 {
        return Err(UnifiedError::bad_request(
            "slug must be between 3 and 64 characters",
        ));
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return Err(UnifiedError::bad_request(
            "slug must not start or end with '-'",
        ));
    }
    if !slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(UnifiedError::bad_request(
            "slug may only contain lower-case letters, digits, and '-'",
        ));
    }
    Ok(slug)
}

/// Validate a PostgreSQL role name used with `SET LOCAL ROLE`:
/// a bare identifier, letters/digits/underscore, not starting with a
/// digit. The value comes from an admin, never from an MCP client,
/// and it is still validated because it is interpolated into SQL as
/// a quoted identifier.
fn validate_role_name(role: &str) -> UnifiedResult<String> {
    let role = role.trim();
    if role.is_empty() || role.chars().count() > 63 {
        return Err(UnifiedError::bad_request(
            "read_only_role must be a non-empty identifier of at most 63 characters",
        ));
    }
    let mut chars = role.chars();
    let first = chars.next().unwrap_or(' ');
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(UnifiedError::bad_request(
            "read_only_role must start with a letter or '_'",
        ));
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(UnifiedError::bad_request(
            "read_only_role may only contain letters, digits, and '_'",
        ));
    }
    Ok(role.to_string())
}

fn validate_tool_list(tools: &[String]) -> UnifiedResult<Vec<String>> {
    if tools.len() > 64 {
        return Err(UnifiedError::bad_request(
            "allowed_tools must contain at most 64 tools",
        ));
    }
    let mut out: Vec<String> = Vec::new();
    for tool in tools {
        let tool = tool.trim();
        if tool.is_empty() {
            return Err(UnifiedError::bad_request(
                "allowed_tools must not contain an empty tool name",
            ));
        }
        if !tool_is_known(tool) {
            return Err(UnifiedError::bad_request(format!(
                "allowed_tools names unknown tool \"{tool}\""
            ))
            .with_metadata("tool", json!(tool)));
        }
        if !out.contains(&tool.to_string()) {
            out.push(tool.to_string());
        }
    }
    Ok(out)
}

fn validate_rate_limit(limit: u32) -> UnifiedResult<u32> {
    if limit == 0 || limit > 100_000 {
        return Err(UnifiedError::bad_request(
            "rate_limit_per_minute must be between 1 and 100000",
        ));
    }
    Ok(limit)
}

/// Recursively remove every denylisted key from a JSON value, as
/// defence in depth on top of the column allowlist: even if a source
/// module hands back a row that contains a credential field, the MCP
/// server never forwards it to a client.
fn scrub_value(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, entry) in map {
                if column_is_denied(&key) {
                    continue;
                }
                out.insert(key, scrub_value(entry));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(scrub_value).collect()),
        other => other,
    }
}

/// Pagination over already-fetched items, in the standard
/// `PageResult` shape.
fn paginate_values(items: Vec<Value>, params: &PaginationParams) -> PageResult<Value> {
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

// ---------------------------------------------------------------------------
// Authentication, rate limiting, entitlement.
// ---------------------------------------------------------------------------

/// Read the presented API key from `Authorization: Bearer <key>` or
/// `X-Api-Key`. The key value itself is never logged and never stored.
fn presented_key(req: &HttpRequest) -> Option<String> {
    if let Some(header) = req.headers().get("authorization") {
        if let Ok(text) = header.to_str() {
            if let Some(key) = text.strip_prefix("Bearer ") {
                let key = key.trim();
                if !key.is_empty() {
                    return Some(key.to_string());
                }
            }
        }
    }
    req.headers()
        .get("x-api-key")
        .and_then(|header| header.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Authenticate an MCP client for one server and one tool.
///
/// Checks, in order: the server is active; a key was presented; the
/// key verifies through the API Keys module; a key with an `owner_id`
/// belongs to the server's owner; the key's scopes grant the tool
/// (`mcp:<tool>`, `mcp:*`, or `*`, via `key_has_scope`); and, for a
/// real tool (not the protocol-level `"read"` check), the server's
/// own `allowed_tools` list permits the tool. The per-key per-minute
/// rate limit (the lower of the key tier and the server limit) is
/// enforced last. On success the key usage is recorded.
///
/// A key scoped only to `mcp:read` may run the protocol methods
/// (`initialize`, `tools/list`, `resources/list`, ...) but may not
/// call any tool: `tools/call` always needs the exact `mcp:<tool>`
/// scope.
fn authenticate_mcp_client(
    ctx: &AppContext,
    state: &McpState,
    req: &HttpRequest,
    server: &McpServer,
    tool: &str,
) -> UnifiedResult<AuthenticatedKey> {
    if server.status != "active" {
        return Err(UnifiedError::forbidden(format!(
            "MCP server \"{}\" is disabled",
            server.slug
        )));
    }
    let presented = presented_key(req)
        .ok_or_else(|| UnifiedError::unauthorized("Missing API key: send Authorization: Bearer <key> or X-Api-Key"))?;
    let authenticated = authenticate_key(&presented)?;
    if let Some(owner_id) = authenticated.owner_id {
        if owner_id != server.owner_id {
            return Err(UnifiedError::forbidden(
                "API key belongs to a different owner than this MCP server",
            ));
        }
    }
    if !key_has_scope(&authenticated.scopes, "mcp", tool) {
        return Err(UnifiedError::forbidden(format!(
            "API key is not scoped to MCP tool \"{tool}\"; grant scope \"mcp:{tool}\" or \"mcp:*\""
        ))
        .with_metadata("tool", json!(tool))
        .with_metadata("scopes", json!(authenticated.scopes)));
    }
    if tool != "read"
        && !server.allowed_tools.is_empty()
        && !server.allowed_tools.iter().any(|entry| entry == tool)
    {
        return Err(UnifiedError::forbidden(format!(
            "MCP server \"{}\" does not allow tool \"{tool}\"",
            server.slug
        ))
        .with_metadata("tool", json!(tool)));
    }
    let limit = authenticated
        .requests_per_minute
        .min(server.rate_limit_per_minute);
    check_rate_limit(state, &authenticated.id, limit)?;
    note_used(authenticated.id);
    ctx.hooks.emit(
        mcp_events::CLIENT_AUTHENTICATED,
        json!({
            "server_id": server.id,
            "key_id": authenticated.id,
            "tool": tool,
        }),
    );
    Ok(authenticated)
}

/// Per-key per-minute counter. The limit value comes from the key's
/// own tier (API Keys module) capped by the server limit; this
/// function only counts, it invents no tier.
fn check_rate_limit(state: &McpState, key_id: &Uuid, limit: u32) -> UnifiedResult<()> {
    let minute = Utc::now().timestamp() / 60;
    let bucket_key = key_id.to_string();
    let mut bucket = state
        .rate_buckets
        .entry(bucket_key)
        .or_insert(RateBucket { minute, count: 0 });
    if bucket.minute != minute {
        bucket.minute = minute;
        bucket.count = 0;
    }
    if bucket.count >= limit {
        return Err(UnifiedError::new(
            "Rate limit exceeded for this API key",
            429,
        )
        .with_metadata("limit_per_minute", json!(limit))
        .with_metadata("retry_after_seconds", json!(60 - (Utc::now().timestamp() % 60))));
    }
    bucket.count += 1;
    Ok(())
}

/// Ask Product Management whether `user_id` is entitled to
/// `product_id`, through the `product.entitlement.check` hook event
/// (`{"product_id", "user_id"}` in, `{"entitled": bool}` out).
/// Fail-closed, like the meetings module's join check: no handler
/// answering is a 503, an explicit `{"entitled": false}` (or an
/// `error` in the result) is a 403.
fn check_entitlement(ctx: &AppContext, product_id: &str, user_id: &str) -> UnifiedResult<()> {
    let answer = ctx.hooks.emit(
        "product.entitlement.check",
        json!({ "product_id": product_id, "user_id": user_id }),
    );
    match answer.results.first() {
        Some(result) if result.get("entitled").and_then(Value::as_bool) == Some(true) => Ok(()),
        Some(result) if result.get("error").is_some() => {
            let reason = result
                .get("error")
                .and_then(|value| value.as_str())
                .unwrap_or("entitlement check failed");
            Err(UnifiedError::new(
                format!("Entitlement check failed: {reason}"),
                502,
            ))
        }
        Some(_) => Err(UnifiedError::forbidden(
            "A product entitlement is required for this MCP server",
        )
        .with_metadata("product_id", json!(product_id))),
        None => Err(UnifiedError::new(
            "Entitlement service did not answer product.entitlement.check: no handler is registered",
            503,
        )),
    }
}

fn resolve_server(state: &McpState, id_or_slug: &str) -> UnifiedResult<McpServer> {
    if let Ok(id) = Uuid::parse_str(id_or_slug) {
        if let Some(server) = state.servers.get(&id) {
            return Ok(server.value().clone());
        }
    }
    let slug = id_or_slug.trim().to_lowercase();
    if let Some(id) = state.servers_by_slug.get(&slug) {
        if let Some(server) = state.servers.get(id.value()) {
            return Ok(server.value().clone());
        }
    }
    Err(UnifiedError::not_found(format!(
        "MCP server \"{id_or_slug}\" not found"
    )))
}

// ---------------------------------------------------------------------------
// Context assembly (hook bus) and read-only database queries.
// ---------------------------------------------------------------------------

/// One page of context from a source.
#[derive(Debug, Clone)]
struct ContextPage {
    items: Vec<Value>,
    total: u64,
    /// False when no module answered the source's hook event (or the
    /// database fallback was unavailable): the caller must say so
    /// instead of presenting an empty list as a complete answer.
    available: bool,
    gap: Option<String>,
}

/// Fetch one context source for a server.
///
/// The owning module is asked through `mcp.context.<source>` with
/// `{"server_id", "company_id", "site_id", "page", "per_page",
/// "search", "filters"}`. A handler answers by contributing
/// `{"items": [...], "total": n}` as a hook result; the first
/// well-formed result wins. When no handler answers and the source
/// has a database fallback table, the read-only query below runs
/// instead. When neither exists, the page is empty with
/// `available: false` and a gap note — the module reports the gap
/// honestly rather than inventing context.
fn fetch_context(
    ctx: &AppContext,
    server: &McpServer,
    source: &str,
    params: &PaginationParams,
) -> ContextPage {
    let pagination = params.normalized();
    // MERGE POINT — shared site context. The AI module owns the one
    // context assembler (`assemble_shared_context` in ai.rs); the MCP
    // module calls it directly for the shared `site` source instead
    // of building a second assembler or asking through a
    // `mcp.context.site` hook. `include_site_hooks` is true, so
    // modules answering `site.context_requested` still contribute
    // their site facts. The per-source `mcp.context.<source>` hook
    // assembly below stays for the MCP-specific sources only.
    if source == "site" {
        let query = pagination.search.clone().unwrap_or_default();
        let shared = assemble_shared_context(
            Some(&ctx.hooks),
            &query,
            &SiteContextInput::default(),
            true,
        );
        let item = scrub_value(json!(shared));
        ctx.hooks.emit(
            mcp_events::CONTEXT_SERVED,
            json!({
                "server_id": server.id,
                "source": source,
                "items": 1,
                "total": 1,
            }),
        );
        return ContextPage {
            items: vec![item],
            total: 1,
            available: true,
            gap: None,
        };
    }
    let event = format!("{}{}", mcp_events::CONTEXT_PREFIX, source);
    if ctx.hooks.has(&event) {
        let answer = ctx.hooks.emit(
            &event,
            json!({
                "server_id": server.id,
                "company_id": server.company_id,
                "site_id": server.site_id,
                "page": pagination.page,
                "per_page": pagination.per_page,
                "search": pagination.search,
                "filters": pagination.filters,
            }),
        );
        for result in &answer.results {
            if let Some(items) = result.get("items").and_then(Value::as_array) {
                let total = result
                    .get("total")
                    .and_then(Value::as_u64)
                    .unwrap_or(items.len() as u64);
                let items: Vec<Value> =
                    items.iter().cloned().map(scrub_value).collect();
                ctx.hooks.emit(
                    mcp_events::CONTEXT_SERVED,
                    json!({
                        "server_id": server.id,
                        "source": source,
                        "items": items.len(),
                        "total": total,
                    }),
                );
                return ContextPage {
                    items,
                    total,
                    available: true,
                    gap: None,
                };
            }
        }
    }
    if let Some(table_name) = find_source(source).and_then(|entry| entry.db_table) {
        if let Some(table) = find_table(table_name) {
            let search = pagination.search.clone().unwrap_or_default();
            match read_only_query_blocking(
                ctx,
                table,
                None,
                &search,
                pagination.page,
                pagination.per_page,
                server.read_only_role.clone(),
            ) {
                Ok(page) => {
                    return ContextPage {
                        items: page.items,
                        total: page.pagination.total,
                        available: true,
                        gap: None,
                    };
                }
                Err(error) => {
                    return ContextPage {
                        items: Vec::new(),
                        total: 0,
                        available: false,
                        gap: Some(format!(
                            "Database fallback for \"{source}\" failed: {}",
                            error.message
                        )),
                    };
                }
            }
        }
    }
    ContextPage {
        items: Vec::new(),
        total: 0,
        available: false,
        gap: Some(format!(
            "No module answers the \"{event}\" hook yet, so the \"{source}\" context source has no data. The owning module ({}) needs to register a handler that answers with {{\"items\": [...], \"total\": n}}.",
            find_source(source).map(|entry| entry.owner_module).unwrap_or("unknown")
        )),
    }
}

/// Parameters for one read-only query, already validated against the
/// allowlist. Only owned strings travel into the blocking task.
#[derive(Debug, Clone)]
struct ReadOnlyQuerySpec {
    table: String,
    columns: Vec<String>,
    search_column: String,
    search: String,
    page: u32,
    per_page: u32,
    read_only_role: Option<String>,
}

/// One row of `row_to_json(...)::text`.
#[derive(Debug, QueryableByName)]
struct RowJson {
    #[diesel(sql_type = Text)]
    row_json: String,
}

/// One row of `SELECT COUNT(*)`.
#[derive(Debug, QueryableByName)]
struct CountRow {
    #[diesel(sql_type = BigInt)]
    total: i64,
}

/// Run one validated read-only query inside a read-only transaction.
///
/// The SQL text is assembled only from allowlist constants (table and
/// column names) and fixed keywords. Every client-supplied value —
/// the search pattern, the limit, and the offset — travels as a bound
/// Diesel parameter, never interpolated into the SQL string. The
/// transaction's first statement is `SET TRANSACTION READ ONLY`, so
/// PostgreSQL refuses any write even if a future code path tried one;
/// when an admin configured a read-only role, `SET LOCAL ROLE` runs
/// first (the role name was validated as a bare identifier at
/// configuration time and is quoted here).
fn run_read_only_query(
    pool: &DbPool,
    spec: &ReadOnlyQuerySpec,
) -> UnifiedResult<PageResult<Value>> {
    let mut connection = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot borrow a database connection: {error}"))
    })?;
    connection.transaction(|conn| {
        if let Some(role) = &spec.read_only_role {
            diesel::sql_query(format!("SET LOCAL ROLE \"{role}\"")).execute(conn)?;
        }
        diesel::sql_query("SET TRANSACTION READ ONLY").execute(conn)?;
        let offset = ((spec.page - 1) * spec.per_page) as i64;
        let limit = spec.per_page as i64;
        let columns = spec
            .columns
            .iter()
            .map(|column| format!("\"{column}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let has_search = !spec.search.trim().is_empty();
        let search_pattern = format!("%{}%", spec.search.trim());
        // Total first, so pagination metadata is exact.
        let total: i64 = if has_search {
            let rows = diesel::sql_query(format!(
                "SELECT COUNT(*) AS total FROM \"{}\" WHERE \"{}\" ILIKE $1",
                spec.table, spec.search_column
            ))
            .bind::<Text, _>(search_pattern.clone())
            .load::<CountRow>(conn)?;
            rows.first().map(|row| row.total).unwrap_or(0)
        } else {
            let rows = diesel::sql_query(format!(
                "SELECT COUNT(*) AS total FROM \"{}\"",
                spec.table
            ))
            .load::<CountRow>(conn)?;
            rows.first().map(|row| row.total).unwrap_or(0)
        };
        // Rows as one JSON document each: a single generic shape works
        // for every allowlisted table, and no per-table model is needed.
        let rows: Vec<RowJson> = if has_search {
            diesel::sql_query(format!(
                "SELECT row_to_json(t)::text AS row_json FROM (SELECT {columns} FROM \"{}\" WHERE \"{}\" ILIKE $3 ORDER BY \"created_at\" DESC LIMIT $1 OFFSET $2) t",
                spec.table, spec.search_column
            ))
            .bind::<BigInt, _>(limit)
            .bind::<BigInt, _>(offset)
            .bind::<Text, _>(search_pattern)
            .load::<RowJson>(conn)?
        } else {
            diesel::sql_query(format!(
                "SELECT row_to_json(t)::text AS row_json FROM (SELECT {columns} FROM \"{}\" ORDER BY \"created_at\" DESC LIMIT $1 OFFSET $2) t",
                spec.table
            ))
            .bind::<BigInt, _>(limit)
            .bind::<BigInt, _>(offset)
            .load::<RowJson>(conn)?
        };
        let mut items: Vec<Value> = Vec::new();
        for row in rows {
            let parsed: Value = serde_json::from_str(&row.row_json).map_err(|error| {
                UnifiedError::internal(format!("Cannot parse database row as JSON: {error}"))
            })?;
            items.push(scrub_value(parsed));
        }
        let pagination = crate::core::pagination::Pagination {
            page: spec.page,
            per_page: spec.per_page,
            sort: None,
            order: "desc".to_string(),
            search: if spec.search.is_empty() {
                None
            } else {
                Some(spec.search.clone())
            },
            filters: Default::default(),
        };
        Ok(PageResult::new(items, total.max(0) as u64, &pagination))
    })
}

/// Validate a `db.query` request against the allowlist and run it.
/// Returns a 503 when no database pool is configured (the hook
/// context path keeps working without a database).
async fn read_only_query(
    ctx: &AppContext,
    table_name: &str,
    columns: Option<Vec<String>>,
    search: &str,
    page: u32,
    per_page: u32,
    read_only_role: Option<String>,
) -> UnifiedResult<PageResult<Value>> {
    let table = find_table(table_name).ok_or_else(|| {
        UnifiedError::bad_request(format!(
            "Table \"{table_name}\" is not in the MCP read-only allowlist"
        ))
        .with_metadata("table", json!(table_name))
        .with_metadata(
            "allowlisted_tables",
            json!(READ_ONLY_TABLES.iter().map(|entry| entry.name).collect::<Vec<_>>()),
        )
    })?;
    let requested: Vec<String> = match columns {
        Some(columns) if !columns.is_empty() => columns,
        _ => table.columns.iter().map(|entry| entry.to_string()).collect(),
    };
    if requested.len() > table.columns.len() {
        return Err(UnifiedError::bad_request("Too many columns requested"));
    }
    for column in &requested {
        if column_is_denied(column) {
            return Err(UnifiedError::forbidden(format!(
                "Column \"{column}\" is denylisted and is never exposed by MCP"
            ))
            .with_metadata("column", json!(column)));
        }
        if !table.columns.contains(&column.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Column \"{column}\" is not allowlisted for table \"{table_name}\""
            ))
            .with_metadata("column", json!(column))
            .with_metadata("table", json!(table_name)));
        }
    }
    let pool = ctx
        .db
        .read()
        .expect("db lock poisoned")
        .clone()
        .ok_or_else(|| {
            UnifiedError::new(
                "No database pool is configured: the read-only database query path is unavailable (hook context still works)",
                503,
            )
            .with_metadata("reason", json!("database_unavailable"))
        })?;
    let spec = ReadOnlyQuerySpec {
        table: table.name.to_string(),
        columns: requested,
        search_column: table.search_column.to_string(),
        search: search.trim().to_string(),
        page: page.max(1),
        per_page: per_page.clamp(1, 100),
        read_only_role,
    };
    // The blocking task owns the spec, so the pool stays Send.
    tokio::task::spawn_blocking(move || run_read_only_query(&pool, &spec))
        .await
        .map_err(|error| {
            UnifiedError::internal(format!("Read-only database task failed: {error}"))
        })?
}

/// Synchronous wrapper used by the hook fallback inside
/// `fetch_context` (which runs on a handler thread, not inside a
/// blocking task). Kept separate from the async `read_only_query` so
/// the fallback never blocks an Actix worker on a pool wait: it only
/// runs when a pool is already configured and fails fast otherwise.
fn read_only_query_blocking(
    ctx: &AppContext,
    table: &ReadOnlyTable,
    columns: Option<Vec<String>>,
    search: &str,
    page: u32,
    per_page: u32,
    read_only_role: Option<String>,
) -> UnifiedResult<PageResult<Value>> {
    let pool = ctx
        .db
        .read()
        .expect("db lock poisoned")
        .clone()
        .ok_or_else(|| {
            UnifiedError::new("No database pool is configured", 503)
                .with_metadata("reason", json!("database_unavailable"))
        })?;
    let spec = ReadOnlyQuerySpec {
        table: table.name.to_string(),
        columns: columns.unwrap_or_else(|| {
            table.columns.iter().map(|entry| entry.to_string()).collect()
        }),
        search_column: table.search_column.to_string(),
        search: search.trim().to_string(),
        page: page.max(1),
        per_page: per_page.clamp(1, 100),
        read_only_role,
    };
    run_read_only_query(&pool, &spec)
}

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 dispatch (the MCP wire protocol).
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[serde(default)]
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

fn rpc_result(id: &Option<Value>, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.clone().unwrap_or(Value::Null),
        "result": result,
    })
}

fn rpc_error(id: &Option<Value>, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut error = json!({
        "code": code,
        "message": message,
    });
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({
        "jsonrpc": "2.0",
        "id": id.clone().unwrap_or(Value::Null),
        "error": error,
    })
}

fn rpc_error_from_unified(id: &Option<Value>, error: &UnifiedError) -> Value {
    rpc_error(
        id,
        JSONRPC_SERVER_ERROR,
        &error.message,
        Some(json!({ "code": error.code, "metadata": error.metadata })),
    )
}

fn arg_string<'a>(args: &'a Value, field: &str) -> Option<&'a str> {
    args.get(field).and_then(Value::as_str)
}

fn arg_u32(args: &Value, field: &str, default: u32) -> u32 {
    args.get(field)
        .and_then(Value::as_u64)
        .map(|value| value as u32)
        .unwrap_or(default)
}

fn arg_page_params(args: &Value) -> PaginationParams {
    PaginationParams {
        page: Some(arg_u32(args, "page", 1)),
        per_page: Some(arg_u32(args, "per_page", 20)),
        sort: None,
        order: None,
        search: arg_string(args, "search").map(str::to_string),
        filters: Default::default(),
    }
}

/// One context page rendered as a tool result: items plus the
/// availability flag, so a client can tell "no data because the
/// source is not wired up" apart from "no data because there is
/// nothing to show".
fn context_tool_result(page: ContextPage, source: &str) -> Value {
    let mut result = json!({
        "source": source,
        "available": page.available,
        "items": page.items,
        "total": page.total,
    });
    if let Some(gap) = page.gap {
        result["gap"] = json!(gap);
    }
    result
}

/// Dispatch one tool call. Every built-in tool is read-only; an
/// unknown name is offered to hook-contributed tools first
/// (`mcp.tool.call_requested`) and otherwise answers "method not
/// found". Write-capable tools do not exist: a client asking for one
/// gets a clear refusal, not a stub.
async fn dispatch_tool(
    ctx: &AppContext,
    server: &McpServer,
    tool: &str,
    args: &Value,
) -> Result<Value, (i64, String, Option<Value>)> {
    let err = |code: i64, message: String| -> (i64, String, Option<Value>) {
        (code, message, None)
    };
    let page_params = arg_page_params(args);
    match tool {
        "site.search" => {
            let query = arg_string(args, "query").ok_or_else(|| {
                err(
                    JSONRPC_INVALID_PARAMS,
                    "site.search requires a \"query\" string".to_string(),
                )
            })?;
            let wanted: Option<Vec<String>> = args
                .get("sources")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|entry| entry.as_str().map(str::to_string))
                        .collect()
                });
            let mut results: Vec<Value> = Vec::new();
            for source in CONTEXT_SOURCES {
                if let Some(wanted) = &wanted {
                    if !wanted.iter().any(|entry| entry == source.name) {
                        continue;
                    }
                }
                let mut params = page_params.clone();
                params.search = Some(query.to_string());
                let page = fetch_context(ctx, server, source.name, &params);
                results.push(context_tool_result(page, source.name));
            }
            Ok(json!({ "query": query, "results": results }))
        }
        "site.context" => {
            let query = arg_string(args, "query").unwrap_or("");
            let mut params = page_params.clone();
            params.search = Some(query.to_string());
            let page = fetch_context(ctx, server, "site", &params);
            Ok(context_tool_result(page, "site"))
        }
        "content.list" | "products.list" | "orders.list" | "tickets.list"
        | "email_templates.list" | "newsletter.list" | "site_builder.pages.list" => {
            let source = match tool {
                "content.list" => "content",
                "products.list" => "products",
                "orders.list" => "orders",
                "tickets.list" => "tickets",
                "email_templates.list" => "email_templates",
                "newsletter.list" => "newsletter",
                _ => "site_builder",
            };
            let page = fetch_context(ctx, server, source, &page_params);
            Ok(context_tool_result(page, source))
        }
        "content.get" | "products.get" => {
            let id = arg_string(args, "id").ok_or_else(|| {
                err(
                    JSONRPC_INVALID_PARAMS,
                    format!("{tool} requires an \"id\" string"),
                )
            })?;
            let source = if tool == "content.get" {
                "content"
            } else {
                "products"
            };
            let mut params = PaginationParams::default();
            params.filters.insert("id".to_string(), id.to_string());
            params.per_page = Some(1);
            let page = fetch_context(ctx, server, source, &params);
            match page.items.into_iter().next() {
                Some(item) => Ok(json!({ "source": source, "available": true, "item": item })),
                None => Err(err(
                    JSONRPC_SOURCE_UNAVAILABLE,
                    format!(
                        "No {source} record with id \"{id}\" was returned{}",
                        page.gap
                            .map(|gap| format!(": {gap}"))
                            .unwrap_or_default()
                    ),
                )),
            }
        }
        "knowledgebase.search" => {
            let query = arg_string(args, "query").ok_or_else(|| {
                err(
                    JSONRPC_INVALID_PARAMS,
                    "knowledgebase.search requires a \"query\" string".to_string(),
                )
            })?;
            let limit = arg_u32(args, "limit", 5).clamp(1, 50);
            if ctx.hooks.has("knowledgebase.search") {
                let answer = ctx.hooks.emit(
                    "knowledgebase.search",
                    json!({ "query": query, "limit": limit }),
                );
                for result in &answer.results {
                    if let Some(articles) = result.get("articles").and_then(Value::as_array) {
                        let articles: Vec<Value> =
                            articles.iter().cloned().map(scrub_value).collect();
                        return Ok(json!({
                            "source": "knowledgebase",
                            "available": true,
                            "articles": articles,
                        }));
                    }
                }
            }
            Ok(json!({
                "source": "knowledgebase",
                "available": false,
                "articles": [],
                "gap": "No module answers the \"knowledgebase.search\" hook yet, so knowledgebase search has no data.",
            }))
        }
        "db.query" => {
            let table = arg_string(args, "table").ok_or_else(|| {
                err(
                    JSONRPC_INVALID_PARAMS,
                    "db.query requires a \"table\" string".to_string(),
                )
            })?;
            let columns: Option<Vec<String>> = args
                .get("columns")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|entry| entry.as_str().map(str::to_string))
                        .collect()
                });
            let search = arg_string(args, "search").unwrap_or("");
            let page = read_only_query(
                ctx,
                table,
                columns,
                search,
                arg_u32(args, "page", 1),
                arg_u32(args, "per_page", 20),
                server.read_only_role.clone(),
            )
            .await
            .map_err(|error| {
                (
                    JSONRPC_SERVER_ERROR,
                    error.message.clone(),
                    Some(json!({ "code": error.code, "metadata": error.metadata })),
                )
            })?;
            Ok(json!({
                "table": table,
                "allowlisted_tables": READ_ONLY_TABLES.iter().map(|entry| entry.name).collect::<Vec<_>>(),
                "read_only": true,
                "items": page.items,
                "pagination": page.pagination,
            }))
        }
        other => {
            // Hook-contributed tool: ask its owning module. The first
            // result carrying "result" wins; anything else is an
            // honest "no handler answered" error, never a stub value.
            let answer = ctx.hooks.emit(
                mcp_events::TOOL_CALL_REQUESTED,
                json!({
                    "server_id": server.id,
                    "tool": other,
                    "arguments": args,
                }),
            );
            for result in &answer.results {
                if let Some(inner) = result.get("result") {
                    return Ok(scrub_value(inner.clone()));
                }
            }
            Err(err(
                JSONRPC_METHOD_NOT_FOUND,
                format!("Unknown tool \"{other}\": no built-in tool and no module answered mcp.tool.call_requested"),
            ))
        }
    }
}

/// Dispatch one JSON-RPC 2.0 request for a server, authenticating
/// the key once (see the note at the top of the function body).
async fn dispatch_rpc(
    ctx: &AppContext,
    state: &McpState,
    req: &HttpRequest,
    server: &McpServer,
    body: JsonRpcRequest,
) -> Value {
    let id = &body.id;
    if body.jsonrpc.as_deref() != Some("2.0") && body.jsonrpc.is_some() {
        return rpc_error(
            id,
            JSONRPC_INVALID_PARAMS,
            "jsonrpc must be \"2.0\"",
            None,
        );
    }
    let params = body.params.clone().unwrap_or(Value::Null);
    // Authentication happens here, once per request, so usage and
    // rate limiting are counted exactly once. Every method needs at
    // least `mcp:read`; `tools/call` instead needs the exact
    // `mcp:<tool>` scope for the tool named in its body, so it
    // authenticates in its own arm and is skipped here.
    if body.method.as_str() != "tools/call" {
        if let Err(error) = authenticate_mcp_client(ctx, state, req, server, "read") {
            return rpc_error_from_unified(id, &error);
        }
    }
    match body.method.as_str() {
        "initialize" => rpc_result(
            id,
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": { "tools": {}, "resources": {}, "prompts": {} },
                "serverInfo": { "name": "unified-mcp", "version": "1.0.0" },
                "instructions": "Read-only MCP server for this site. Every tool, resource, and prompt is read-only; there are no write-capable tools. Database reads are limited to the allowlisted tables in the server config, run inside read-only transactions, and never expose credential columns.",
            }),
        ),
        "ping" => rpc_result(id, json!({})),
        "tools/list" => {
            let mut tools: Vec<Value> = builtin_tools()
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.name,
                        "description": tool.description,
                        "inputSchema": tool.input_schema,
                    })
                })
                .collect();
            // Other modules may contribute extra read-only tools.
            let answer = ctx.hooks.emit(
                mcp_events::TOOLS_EXTEND,
                json!({ "server_id": server.id }),
            );
            for result in &answer.results {
                if let Some(extra) = result.get("tools").and_then(Value::as_array) {
                    for entry in extra {
                        let name = entry.get("name").and_then(Value::as_str()).unwrap_or("");
                        if name.is_empty()
                            || !name
                                .chars()
                                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' || c == '.')
                            || tools.iter().any(|tool| tool.get("name").and_then(Value::as_str()) == Some(name))
                        {
                            continue;
                        }
                        tools.push(json!({
                            "name": name,
                            "description": entry.get("description").and_then(Value::as_str()).unwrap_or("Contributed read-only tool."),
                            "inputSchema": entry.get("inputSchema").cloned().unwrap_or(json!({ "type": "object", "properties": {} })),
                        }));
                    }
                }
            }
            rpc_result(id, json!({ "tools": tools }))
        }
        "tools/call" => {
            let name = match params.get("name").and_then(Value::as_str()) {
                Some(name) => name.to_string(),
                None => {
                    return rpc_error(
                        id,
                        JSONRPC_INVALID_PARAMS,
                        "tools/call requires params.name",
                        None,
                    );
                }
            };
            // Per-tool scope: the key must be scoped to this exact
            // tool (the generic "mcp:read" check at the door is not
            // enough to call a tool).
            match authenticate_mcp_client(ctx, state, req, server, &name) {
                Ok(_) => {}
                Err(error) => return rpc_error_from_unified(id, &error),
            }
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            if !args.is_object() {
                return rpc_error(
                    id,
                    JSONRPC_INVALID_PARAMS,
                    "tools/call arguments must be a JSON object",
                    None,
                );
            }
            match dispatch_tool(ctx, server, &name, &args).await {
                Ok(result) => {
                    ctx.hooks.emit(
                        mcp_events::TOOL_CALLED,
                        json!({
                            "server_id": server.id,
                            "tool": name,
                        }),
                    );
                    rpc_result(
                        id,
                        json!({
                            "content": [{ "type": "text", "text": serde_json::to_string(&result).unwrap_or_default() }],
                            "structuredContent": result,
                        }),
                    )
                }
                Err((code, message, data)) => rpc_error(id, code, &message, data),
            }
        }
        "resources/list" => {
            let resources: Vec<Value> = RESOURCES
                .iter()
                .map(|(uri, description, source)| {
                    json!({
                        "uri": uri,
                        "name": uri,
                        "description": description,
                        "mimeType": "application/json",
                        "_source": source,
                    })
                })
                .collect();
            rpc_result(id, json!({ "resources": resources }))
        }
        "resources/read" => {
            let uri = match params.get("uri").and_then(Value::as_str()) {
                Some(uri) => uri.to_string(),
                None => {
                    return rpc_error(
                        id,
                        JSONRPC_INVALID_PARAMS,
                        "resources/read requires params.uri",
                        None,
                    );
                }
            };
            let Some((_, _, source)) = RESOURCES.iter().find(|(candidate, _, _)| *candidate == uri) else {
                return rpc_error(
                    id,
                    JSONRPC_METHOD_NOT_FOUND,
                    &format!("Unknown resource \"{uri}\""),
                    None,
                );
            };
            let page = fetch_context(ctx, server, source, &PaginationParams::default());
            rpc_result(
                id,
                json!({
                    "contents": [{
                        "uri": uri,
                        "mimeType": "application/json",
                        "text": serde_json::to_string(&context_tool_result(page, source)).unwrap_or_default(),
                    }],
                }),
            )
        }
        "prompts/list" => {
            let prompts: Vec<Value> = PROMPTS
                .iter()
                .map(|prompt| {
                    json!({
                        "name": prompt.name,
                        "description": prompt.description,
                        "arguments": prompt.arguments.iter().map(|(name, description, required)| {
                            json!({ "name": name, "description": description, "required": required })
                        }).collect::<Vec<_>>(),
                    })
                })
                .collect();
            rpc_result(id, json!({ "prompts": prompts }))
        }
        "prompts/get" => {
            let name = match params.get("name").and_then(Value::as_str()) {
                Some(name) => name.to_string(),
                None => {
                    return rpc_error(
                        id,
                        JSONRPC_INVALID_PARAMS,
                        "prompts/get requires params.name",
                        None,
                    );
                }
            };
            let Some(prompt) = PROMPTS.iter().find(|entry| entry.name == name) else {
                return rpc_error(
                    id,
                    JSONRPC_METHOD_NOT_FOUND,
                    &format!("Unknown prompt \"{name}\""),
                    None,
                );
            };
            let supplied = params.get("arguments").cloned().unwrap_or(json!({}));
            let mut text = prompt.template.to_string();
            for (arg_name, _, required) in prompt.arguments {
                match supplied.get(arg_name).and_then(Value::as_str) {
                    Some(value) => {
                        text = text.replace(&format!("{{{{{arg_name}}}}}"), value);
                    }
                    None if *required => {
                        return rpc_error(
                            id,
                            JSONRPC_INVALID_PARAMS,
                            &format!("Prompt \"{name}\" requires argument \"{arg_name}\""),
                            None,
                        );
                    }
                    None => {
                        text = text.replace(&format!("{{{{{arg_name}}}}}"), "");
                    }
                }
            }
            rpc_result(
                id,
                json!({
                    "description": prompt.description,
                    "messages": [{ "role": "user", "content": { "type": "text", "text": text } }],
                }),
            )
        }
        other => rpc_error(
            id,
            JSONRPC_METHOD_NOT_FOUND,
            &format!("Unknown method \"{other}\""),
            None,
        ),
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers: admin REST surface (standard envelope) and the MCP
// endpoint (JSON-RPC 2.0, per the MCP wire protocol).
// ---------------------------------------------------------------------------

async fn list_servers(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<McpState>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<Value> = state
            .servers
            .iter()
            .map(|entry| entry.value().public_view())
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|server| {
                server
                    .get("name")
                    .and_then(Value::as_str)
                    .map(|name| name.to_lowercase().contains(&needle))
                    .unwrap_or(false)
                    || server
                        .get("slug")
                        .and_then(Value::as_str)
                        .map(|slug| slug.contains(&needle))
                        .unwrap_or(false)
            });
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|server| {
                server.get("status").and_then(Value::as_str) == Some(status.as_str())
            });
        }
        if let Some(owner) = params.filters.get("owner_id") {
            items.retain(|server| {
                server.get("owner_id").and_then(Value::as_str) == Some(owner.as_str())
            });
        }
        if let Some(product) = params.filters.get("product_id") {
            items.retain(|server| {
                server.get("product_id").and_then(Value::as_str) == Some(product.as_str())
            });
        }
        if let Some(public) = params.filters.get("is_public") {
            let wanted = public == "true";
            items.retain(|server| {
                server.get("is_public").and_then(Value::as_bool) == Some(wanted)
            });
        }
        let sort = params.allowed_sort(&["created_at", "name", "slug"], "created_at");
        match sort.as_str() {
            "name" => items.sort_by(|a, b| {
                a.get("name").and_then(Value::as_str).cmp(&b.get("name").and_then(Value::as_str))
            }),
            "slug" => items.sort_by(|a, b| {
                a.get("slug").and_then(Value::as_str).cmp(&b.get("slug").and_then(Value::as_str))
            }),
            _ => items.sort_by(|a, b| {
                a.get("created_at").and_then(Value::as_str).cmp(&b.get("created_at").and_then(Value::as_str))
            }),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate_values(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn create_server(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<McpState>>,
    body: web::Json<CreateServerRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let name = body.name.trim().to_string();
        if name.is_empty() || name.chars().count() > 120 {
            return Err(UnifiedError::bad_request(
                "name is required and must be at most 120 characters",
            ));
        }
        let slug = validate_slug(&body.slug)?;
        if state.servers_by_slug.contains_key(&slug) {
            return Err(UnifiedError::conflict(format!(
                "An MCP server with slug \"{slug}\" already exists"
            )));
        }
        let product_id = body.product_id.as_deref().map(str::trim).filter(|value| !value.is_empty()).map(str::to_string);
        if let Some(product_id) = &product_id {
            // Selling the server: the buyer must hold the product
            // entitlement. Fail-closed when Product Management does
            // not answer.
            check_entitlement(&ctx, product_id, &body.owner_id.to_string())?;
        }
        let allowed_tools = match &body.allowed_tools {
            Some(tools) => validate_tool_list(tools)?,
            None => Vec::new(),
        };
        let rate_limit = match body.rate_limit_per_minute {
            Some(limit) => validate_rate_limit(limit)?,
            None => state
                .config
                .read()
                .expect("mcp config lock poisoned")
                .default_rate_limit_per_minute,
        };
        let read_only_role = match &body.read_only_role {
            Some(role) => Some(validate_role_name(role)?),
            None => None,
        };
        let now = Utc::now();
        let server = McpServer {
            id: Uuid::now_v7(),
            slug: slug.clone(),
            name,
            owner_id: body.owner_id,
            company_id: body.company_id,
            site_id: body.site_id,
            product_id,
            status: "active".to_string(),
            is_public: body.is_public,
            allowed_tools,
            rate_limit_per_minute: rate_limit,
            read_only_role,
            created_at: now,
            updated_at: now,
        };
        let view = server.public_view();
        state.servers_by_slug.insert(slug, server.id);
        state.servers.insert(server.id, server.clone());
        ctx.hooks.emit(
            mcp_events::SERVER_CREATED,
            json!({
                "server_id": server.id,
                "slug": server.slug,
                "owner_id": server.owner_id,
                "product_id": server.product_id,
                "is_public": server.is_public,
            }),
        );
        Ok(view)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_server(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<McpState>>,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(
        resolve_server(&state, &id).map(|server| server.public_view()),
        debug,
    )
}

async fn update_server(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<McpState>>,
    id: web::Path<String>,
    body: web::Json<UpdateServerRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let server = resolve_server(&state, &id)?;
        if let Some(name) = &body.name {
            let name = name.trim();
            if name.is_empty() || name.chars().count() > 120 {
                return Err(UnifiedError::bad_request(
                    "name must be non-empty and at most 120 characters",
                ));
            }
        }
        if let Some(status) = &body.status {
            if status != "active" && status != "disabled" {
                return Err(UnifiedError::bad_request(
                    "status must be \"active\" or \"disabled\"",
                ));
            }
        }
        let allowed_tools = match &body.allowed_tools {
            Some(tools) => Some(validate_tool_list(tools)?),
            None => None,
        };
        let rate_limit = match body.rate_limit_per_minute {
            Some(limit) => Some(validate_rate_limit(limit)?),
            None => None,
        };
        let read_only_role = match &body.read_only_role {
            Some(Some(role)) => Some(Some(validate_role_name(role)?)),
            Some(None) => Some(None),
            None => None,
        };
        // Changing the linked product re-checks the entitlement
        // against the same owner; a sold server cannot be silently
        // re-pointed at a product its owner does not hold.
        if let Some(Some(product_id)) = &body.product_id {
            let product_id = product_id.trim();
            if !product_id.is_empty() {
                check_entitlement(&ctx, product_id, &server.owner_id.to_string())?;
            }
        }
        let mut record = state
            .servers
            .get_mut(&server.id)
            .ok_or_else(|| UnifiedError::not_found("MCP server not found"))?;
        if let Some(name) = &body.name {
            record.name = name.trim().to_string();
        }
        if let Some(status) = &body.status {
            record.status = status.clone();
        }
        if let Some(is_public) = body.is_public {
            record.is_public = is_public;
        }
        if let Some(tools) = allowed_tools {
            record.allowed_tools = tools;
        }
        if let Some(limit) = rate_limit {
            record.rate_limit_per_minute = limit;
        }
        if let Some(role) = read_only_role {
            record.read_only_role = role;
        }
        if let Some(product_id) = &body.product_id {
            record.product_id = product_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
        }
        record.updated_at = Utc::now();
        let view = record.public_view();
        drop(record);
        ctx.hooks.emit(
            mcp_events::SERVER_UPDATED,
            json!({ "server_id": server.id, "slug": server.slug }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

async fn delete_server(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<McpState>>,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let server = resolve_server(&state, &id)?;
        state.servers.remove(&server.id);
        state.servers_by_slug.remove(&server.slug);
        ctx.hooks.emit(
            mcp_events::SERVER_DELETED,
            json!({ "server_id": server.id, "slug": server.slug }),
        );
        Ok(json!({ "deleted": true, "server_id": server.id, "slug": server.slug }))
    })();
    json::respond(result, debug)
}

/// Public registry view of one server: what a buyer or a public
/// client may see before authenticating. No role name, no owner
/// internals beyond the public scope, and never a key.
async fn get_server_public(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<McpState>>,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let server = resolve_server(&state, &id)?;
        if !server.is_public {
            return Err(UnifiedError::not_found(format!(
                "MCP server \"{}\" is not public",
                server.slug
            )));
        }
        if server.status != "active" {
            return Err(UnifiedError::forbidden(format!(
                "MCP server \"{}\" is disabled",
                server.slug
            )));
        }
        Ok(json!({
            "id": server.id,
            "slug": server.slug,
            "name": server.name,
            "company_id": server.company_id,
            "site_id": server.site_id,
            "status": server.status,
            "is_public": true,
            "endpoint": format!("/api/v1/mcp/servers/{}/mcp", server.slug),
            "public_endpoint": format!("/v1/mcp/servers/{}/mcp", server.slug),
            "protocol_version": MCP_PROTOCOL_VERSION,
            "read_only": true,
            "tools": builtin_tools().iter().map(|tool| tool.name).collect::<Vec<_>>(),
            "resources": RESOURCES.iter().map(|(uri, _, _)| uri).collect::<Vec<_>>(),
            "prompts": PROMPTS.iter().map(|prompt| prompt.name).collect::<Vec<_>>(),
        }))
    })();
    json::respond(result, debug)
}

async fn get_config(ctx: web::Data<Arc<AppContext>>, state: web::Data<Arc<McpState>>) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(Ok::<McpConfigView, UnifiedError>(state.config_view(&ctx)), debug)
}

async fn update_config(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<McpState>>,
    body: web::Json<UpdateConfigRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<McpConfigView> = (|| {
        let read_only_role = match &body.read_only_role {
            Some(Some(role)) => Some(Some(validate_role_name(role)?)),
            Some(None) => Some(None),
            None => None,
        };
        let default_rate = match body.default_rate_limit_per_minute {
            Some(limit) => Some(validate_rate_limit(limit)?),
            None => None,
        };
        {
            let mut config = state.config.write().expect("mcp config lock poisoned");
            if let Some(role) = read_only_role {
                config.read_only_role = role;
            }
            if let Some(limit) = default_rate {
                config.default_rate_limit_per_minute = limit;
            }
        }
        Ok(state.config_view(&ctx))
    })();
    json::respond(result, debug)
}

async fn list_tools(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    let tools: Vec<Value> = builtin_tools()
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": tool.input_schema,
                "read_only": true,
            })
        })
        .collect();
    json::respond(
        Ok::<Value, UnifiedError>(json!({
            "tools": tools,
            "read_only": true,
            "writes_supported": false,
            "writes_note": "MVP MCP is read-only. Write-capable tools are a future, admin-gated feature.",
        })),
        debug,
    )
}

async fn list_resources(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    let resources: Vec<Value> = RESOURCES
        .iter()
        .map(|(uri, description, source)| {
            json!({
                "uri": uri,
                "description": description,
                "source": source,
                "mimeType": "application/json",
            })
        })
        .collect();
    json::respond(Ok::<Value, UnifiedError>(json!({ "resources": resources })), debug)
}

async fn list_prompts(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    let prompts: Vec<Value> = PROMPTS
        .iter()
        .map(|prompt| {
            json!({
                "name": prompt.name,
                "description": prompt.description,
                "arguments": prompt.arguments.iter().map(|(name, description, required)| {
                    json!({ "name": name, "description": description, "required": required })
                }).collect::<Vec<_>>(),
            })
        })
        .collect();
    json::respond(Ok::<Value, UnifiedError>(json!({ "prompts": prompts })), debug)
}

/// The MCP endpoint itself. Authentication happens inside
/// `dispatch_rpc`, once per request (a `mcp:read` scope for every
/// method, the exact `mcp:<tool>` scope for `tools/call`), so usage
/// and rate limiting are counted exactly once. The response is
/// JSON-RPC 2.0, not the standard envelope, because that is the MCP
/// wire protocol; a malformed (non-JSON-RPC) body is still answered
/// with the standard error envelope, since no JSON-RPC id exists to
/// echo.
async fn mcp_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<McpState>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<JsonRpcRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let server = match resolve_server(&state, &id) {
        Ok(server) => server,
        Err(error) => return json::error_response(&error, debug),
    };
    if server.status != "active" {
        return json::error_response(
            &UnifiedError::forbidden(format!("MCP server \"{}\" is disabled", server.slug)),
            debug,
        );
    }
    let response = dispatch_rpc(&ctx, &state, &req, &server, body.into_inner()).await;
    HttpResponse::Ok().json(response)
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct McpModule {
    state: Arc<McpState>,
}

impl McpModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(McpState::default()),
        }
    }
}

impl Default for McpModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for McpModule {
    fn id(&self) -> &'static str {
        "mcp"
    }

    fn name(&self) -> &'static str {
        "MCP Server"
    }

    fn description(&self) -> &'static str {
        "Model Context Protocol server for the site's own context: scoped per-site servers (a sellable product, gated by the product entitlement hook), read-only tools, resources, and prompts over content, products, orders, tickets, knowledgebase, settings, email templates, newsletter, and site-builder pages, plus allowlisted read-only database queries. API-key authenticated with per-key tool scopes; public servers are published through the API Manager registry."
    }

    fn requires(&self) -> &[&'static str] {
        &["api-manager", "apikeys", "products", "store"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["diesel", "serde_json"]
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Answer `mcp.server.exists` for other modules with
        // `{"exists": bool}`. Payload: `{"server_id"}` (uuid) or
        // `{"slug"}`.
        let state = self.state.clone();
        bus.register(
            "mcp.server.exists",
            "mcp",
            Arc::new(move |ctx: &mut HookContext| {
                let id_or_slug = ctx
                    .get("server_id")
                    .and_then(|value| value.as_str())
                    .or_else(|| ctx.get("slug").and_then(|value| value.as_str()))
                    .unwrap_or("")
                    .to_string();
                let exists = !id_or_slug.trim().is_empty()
                    && resolve_server(&state, &id_or_slug).is_ok();
                ctx.add_result(json!({
                    "exists": exists,
                    "server_id": id_or_slug,
                }));
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/mcp")
                .app_data(state)
                .route("/servers", web::get().to(list_servers))
                .route("/servers", web::post().to(create_server))
                .route("/servers/{id}", web::get().to(get_server))
                .route("/servers/{id}", web::put().to(update_server))
                .route("/servers/{id}", web::delete().to(delete_server))
                .route("/servers/{id}/public", web::get().to(get_server_public))
                .route("/servers/{id}/mcp", web::post().to(mcp_endpoint))
                .route("/config", web::get().to(get_config))
                .route("/config", web::put().to(update_config))
                .route("/tools", web::get().to(list_tools))
                .route("/resources", web::get().to(list_resources))
                .route("/prompts", web::get().to(list_prompts)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/mcp/servers", owner: "mcp", description: "List MCP servers (paginated, filter by status, owner, product, public)" },
            RouteInfo { method: "POST", path: "/api/v1/mcp/servers", owner: "mcp", description: "Create an MCP server; product-linked servers check the product entitlement hook" },
            RouteInfo { method: "GET", path: "/api/v1/mcp/servers/{id}", owner: "mcp", description: "Get one MCP server" },
            RouteInfo { method: "PUT", path: "/api/v1/mcp/servers/{id}", owner: "mcp", description: "Update an MCP server" },
            RouteInfo { method: "DELETE", path: "/api/v1/mcp/servers/{id}", owner: "mcp", description: "Delete an MCP server" },
            RouteInfo { method: "GET", path: "/api/v1/mcp/servers/{id}/public", owner: "mcp", description: "Public registry view of one public MCP server" },
            RouteInfo { method: "POST", path: "/api/v1/mcp/servers/{id}/mcp", owner: "mcp", description: "MCP JSON-RPC 2.0 endpoint for one server (API key, per-tool scopes)" },
            RouteInfo { method: "GET", path: "/api/v1/mcp/config", owner: "mcp", description: "MCP configuration: read-only allowlist, column denylist, context sources" },
            RouteInfo { method: "PUT", path: "/api/v1/mcp/config", owner: "mcp", description: "Update MCP configuration (read-only role, default rate limit)" },
            RouteInfo { method: "GET", path: "/api/v1/mcp/tools", owner: "mcp", description: "List MCP tools (all read-only)" },
            RouteInfo { method: "GET", path: "/api/v1/mcp/resources", owner: "mcp", description: "List MCP resources" },
            RouteInfo { method: "GET", path: "/api/v1/mcp/prompts", owner: "mcp", description: "List MCP prompts" },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(McpModule::new()));
}
