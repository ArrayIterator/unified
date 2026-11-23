//! mcp - module.

use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use crate::modules::ai;
use actix_web::web;
use serde_json::json;
use std::sync::Arc;
use super::state::*;
use super::validation::*;
use super::ai_capabilities::AI_CAPABILITIES;
use super::handlers::{create_server, delete_server, get_config, get_server, get_server_public, list_prompts, list_resources, list_servers, list_tools, mcp_endpoint, update_config, update_server};

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
    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("mcp", AI_CAPABILITIES);
        Ok(())
    }

    fn id(&self) -> &'static str {
        "mcp"
    }

    fn name(&self) -> &'static str {
        "MCP Server"
    }

    fn description(&self) -> &'static str {
        "Model Context Protocol server for the site's own context: scoped per-site servers (a sellable product, gated by the product entitlement hook), read-only tools, resources, and prompts over content, products, orders, tickets, knowledgebase, settings, email templates, newsletter, and site-builder pages, plus allowlisted read-only database queries. API-key authenticated with per-key tool scopes; public servers are published through the API Manager registry."
    }

    /// MCP is sold as a product: a customer buys a module
    /// entitlement for it (see `crate::modules::licensing`).
    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("ai")
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
            RouteInfo { method: "GET", path: "/api/v1/mcp/servers", owner: "mcp", description: "List MCP servers (paginated, filter by status, owner, product, public)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/mcp/servers", owner: "mcp", description: "Create an MCP server; product-linked servers check the product entitlement hook", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/mcp/servers/{id}", owner: "mcp", description: "Get one MCP server", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/mcp/servers/{id}", owner: "mcp", description: "Update an MCP server", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/mcp/servers/{id}", owner: "mcp", description: "Delete an MCP server", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/mcp/servers/{id}/public", owner: "mcp", description: "Public registry view of one public MCP server", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/mcp/servers/{id}/mcp", owner: "mcp", description: "MCP JSON-RPC 2.0 endpoint for one server (API key, per-tool scopes)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/mcp/config", owner: "mcp", description: "MCP configuration: read-only allowlist, column denylist, context sources", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/mcp/config", owner: "mcp", description: "Update MCP configuration (read-only role, default rate limit)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/mcp/tools", owner: "mcp", description: "List MCP tools (all read-only)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/mcp/resources", owner: "mcp", description: "List MCP resources", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/mcp/prompts", owner: "mcp", description: "List MCP prompts", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(McpModule::new()));
}
