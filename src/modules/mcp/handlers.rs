//! mcp - handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;
use super::protocol::*;
use super::catalog::*;
use super::state::*;
use super::validation::*;
use super::rpc::*;

pub(crate) async fn list_servers(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<McpState>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MCP_READ)?;
        let mut items: Vec<Value> = state
            .servers
            .iter()
            .filter(|entry| in_company_scope(&entry.value().company_id, &scope))
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

pub(crate) async fn create_server(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<McpState>>,
    req: HttpRequest,
    body: web::Json<CreateServerRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MCP_MANAGE)?;
        let company_id = require_company_match_uuid(&scope, &body.company_id)?;
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
            // No list given: store the full built-in list explicitly.
            // An explicitly empty list stays empty and permits no
            // tool (fail closed in `authenticate_mcp_client`).
            None => default_allowed_tools(),
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
            company_id,
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

pub(crate) async fn get_server(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<McpState>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MCP_READ)?;
        Ok(require_server_in_scope(&state, &id, &scope)?.public_view())
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_server(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<McpState>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<UpdateServerRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MCP_MANAGE)?;
        let server = require_server_in_scope(&state, &id, &scope)?;
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

pub(crate) async fn delete_server(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<McpState>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MCP_MANAGE)?;
        let server = require_server_in_scope(&state, &id, &scope)?;
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
pub(crate) async fn get_server_public(
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
            "protocol_version": MCP_PROTOCOL_VERSION,
            "read_only": true,
            "tools": builtin_tools().iter().map(|tool| tool.name).collect::<Vec<_>>(),
            "resources": RESOURCES.iter().map(|(uri, _, _)| uri).collect::<Vec<_>>(),
            "prompts": PROMPTS.iter().map(|prompt| prompt.name).collect::<Vec<_>>(),
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_config(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<McpState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<McpConfigView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MCP_READ)?;
        Ok(state.config_view(&ctx))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_config(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<McpState>>,
    req: HttpRequest,
    body: web::Json<UpdateConfigRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<McpConfigView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MCP_MANAGE)?;
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

pub(crate) async fn list_tools(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
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

pub(crate) async fn list_resources(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
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

pub(crate) async fn list_prompts(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
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
/// and rate limiting are counted exactly once.
///
/// SANCTIONED CARVE-OUT from the standard API envelope
/// (`unified_core::json`): the response is JSON-RPC 2.0, not
/// `{"data": ...}`, because that is the MCP wire protocol — wrapping
/// a JSON-RPC response in the envelope would break every MCP client.
/// A malformed (non-JSON-RPC) body is still answered with the
/// standard error envelope, since no JSON-RPC id exists to echo.
/// See the carve-outs list in `crates/unified-core/src/json.rs`.
pub(crate) async fn mcp_endpoint(
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
