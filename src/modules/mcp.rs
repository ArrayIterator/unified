// MCP server module -- Model Context Protocol access to the site's own context.
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
// out -- answered by the products module). The check is fail-closed:
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


// Constants.

pub mod ai_capabilities;
pub mod protocol;
pub mod catalog;
pub mod state;
pub mod validation;
mod context;
mod rpc;
mod handlers;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::protocol::*;
pub use self::catalog::*;
pub use self::state::*;
pub use self::validation::*;
pub use self::context::*;
pub use self::rpc::*;
pub use self::handlers::*;
pub use self::module::*;
