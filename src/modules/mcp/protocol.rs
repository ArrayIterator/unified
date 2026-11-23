//! mcp - protocol.

/// MCP protocol version this server speaks (2024-11-05).
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// JSON-RPC error code: method not found.
pub(crate) const JSONRPC_METHOD_NOT_FOUND: i64 = -32_601;

/// JSON-RPC error code: invalid params.
pub(crate) const JSONRPC_INVALID_PARAMS: i64 = -32_602;

/// JSON-RPC error code: internal / source error.
pub(crate) const JSONRPC_SERVER_ERROR: i64 = -32_000;

/// JSON-RPC error code: a context source (or tool provider) did not answer.
pub(crate) const JSONRPC_SOURCE_UNAVAILABLE: i64 = -32_002;

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
