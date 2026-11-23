//! mcp - rpc.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::pagination::PaginationParams;
use actix_web::HttpRequest;
use serde::Deserialize;
use serde_json::{json, Value};
use super::protocol::*;
use super::catalog::*;
use super::state::*;
use super::validation::*;
use super::context::*;

#[derive(Debug, Deserialize)]
pub(crate) struct JsonRpcRequest {
    #[serde(default)]
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

pub(crate) fn rpc_result(id: &Option<Value>, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.clone().unwrap_or(Value::Null),
        "result": result,
    })
}

pub(crate) fn rpc_error(id: &Option<Value>, code: i64, message: &str, data: Option<Value>) -> Value {
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

pub(crate) fn rpc_error_from_unified(id: &Option<Value>, error: &UnifiedError) -> Value {
    rpc_error(
        id,
        JSONRPC_SERVER_ERROR,
        &error.message,
        Some(json!({ "code": error.code, "metadata": error.metadata })),
    )
}

pub(crate) fn arg_string<'a>(args: &'a Value, field: &str) -> Option<&'a str> {
    args.get(field).and_then(Value::as_str)
}

pub(crate) fn arg_u32(args: &Value, field: &str, default: u32) -> u32 {
    args.get(field)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(default)
}

pub(crate) fn arg_page_params(args: &Value) -> PaginationParams {
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
pub(crate) fn context_tool_result(page: ContextPage, source: &str) -> Value {
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
pub(crate) async fn dispatch_tool(
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
            // Hook-contributed tool: ask its owning module. A result
            // is trusted only when it explicitly declares itself
            // read-only (`{"result": ..., "read_only": true}`): the
            // module advertises that write-capable tools do not
            // exist, so a contributed tool that will not make that
            // declaration is refused, never run blind. The first
            // qualifying result wins; a handler that answered
            // without the declaration is a refusal, while no
            // handler answering at all is "method not found".
            let answer = ctx.hooks.emit(
                mcp_events::TOOL_CALL_REQUESTED,
                json!({
                    "server_id": server.id,
                    "tool": other,
                    "arguments": args,
                }),
            );
            let mut answered = false;
            for result in &answer.results {
                let Some(inner) = result.get("result") else {
                    continue;
                };
                answered = true;
                if result.get("read_only").and_then(Value::as_bool) == Some(true) {
                    return Ok(scrub_value(inner.clone()));
                }
            }
            if answered {
                return Err(err(
                    JSONRPC_SERVER_ERROR,
                    format!("Contributed tool \"{other}\" did not declare read_only: true, so it was refused"),
                ));
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
pub(crate) async fn dispatch_rpc(
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
            // Only a tool that explicitly declares `read_only: true`
            // is advertised: an undeclared contribution is never
            // listed as a read-only tool.
            let answer = ctx.hooks.emit(
                mcp_events::TOOLS_EXTEND,
                json!({ "server_id": server.id }),
            );
            for result in &answer.results {
                if let Some(extra) = result.get("tools").and_then(Value::as_array) {
                    for entry in extra {
                        let name = entry.get("name").and_then(Value::as_str).unwrap_or("");
                        if name.is_empty()
                            || entry.get("read_only").and_then(Value::as_bool) != Some(true)
                            || !name
                                .chars()
                                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' || c == '.')
                            || tools.iter().any(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
                        {
                            continue;
                        }
                        tools.push(json!({
                            "name": name,
                            "description": entry.get("description").and_then(Value::as_str).unwrap_or("Contributed read-only tool."),
                            "inputSchema": entry.get("inputSchema").cloned().unwrap_or(json!({ "type": "object", "properties": {} })),
                            "read_only": true,
                        }));
                    }
                }
            }
            rpc_result(id, json!({ "tools": tools }))
        }
        "tools/call" => {
            let name = match params.get("name").and_then(Value::as_str) {
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
            let uri = match params.get("uri").and_then(Value::as_str) {
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
            let name = match params.get("name").and_then(Value::as_str) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_result_wraps_result_with_version_and_id() {
        let out = rpc_result(&Some(json!(7)), json!({"ok": true}));
        assert_eq!(out["jsonrpc"], json!("2.0"));
        assert_eq!(out["id"], json!(7));
        assert_eq!(out["result"]["ok"], json!(true));
    }

    #[test]
    fn rpc_result_uses_null_id_when_absent() {
        let out = rpc_result(&None, json!(true));
        assert!(out["id"].is_null());
    }

    #[test]
    fn rpc_error_carries_code_and_message_without_data() {
        let out = rpc_error(&Some(json!(1)), -32601, "Method not found", None);
        assert_eq!(out["error"]["code"], json!(-32601));
        assert_eq!(out["error"]["message"], json!("Method not found"));
        assert!(out["error"].get("data").is_none());
    }

    #[test]
    fn rpc_error_includes_data_when_given() {
        let out = rpc_error(&None, -32602, "Invalid params", Some(json!({"x": 1})));
        assert_eq!(out["error"]["data"]["x"], json!(1));
    }

    #[test]
    fn arg_string_reads_string_args_only() {
        let args = json!({"a": "b", "n": 3});
        assert_eq!(arg_string(&args, "a"), Some("b"));
        assert_eq!(arg_string(&args, "missing"), None);
        assert_eq!(arg_string(&args, "n"), None);
    }

    #[test]
    fn arg_u32_falls_back_to_default() {
        let args = json!({"page": 3, "bad": "x"});
        assert_eq!(arg_u32(&args, "page", 5), 3);
        assert_eq!(arg_u32(&args, "missing", 5), 5);
        assert_eq!(arg_u32(&args, "bad", 5), 5);
    }

    #[test]
    fn arg_u32_does_not_truncate_a_large_u64() {
        // 2^32 + 5 used to wrap to 5 via `as u32`; it must now fall
        // back to the default instead.
        let args = json!({"page": 4_294_967_301_u64});
        assert_eq!(arg_u32(&args, "page", 1), 1);
        let args = json!({"page": 4_294_967_295_u64});
        assert_eq!(arg_u32(&args, "page", 1), u32::MAX);
    }

    #[test]
    fn arg_page_params_maps_paging_args_and_defaults() {
        let args = json!({"page": 2, "per_page": 5, "search": "x"});
        let params = arg_page_params(&args);
        assert_eq!(params.page, Some(2));
        assert_eq!(params.per_page, Some(5));
        assert_eq!(params.search.as_deref(), Some("x"));
        let defaults = arg_page_params(&json!({}));
        assert_eq!(defaults.page, Some(1));
        assert_eq!(defaults.per_page, Some(20));
        assert_eq!(defaults.search, None);
    }
}

#[cfg(test)]
mod dispatch_tests {
    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use chrono::Utc;
    use std::sync::Arc;
    use uuid::Uuid;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn test_server() -> McpServer {
        let now = Utc::now();
        McpServer {
            id: Uuid::new_v4(),
            slug: "test-server".to_string(),
            name: "Test".to_string(),
            owner_id: Uuid::new_v4(),
            company_id: None,
            site_id: None,
            product_id: None,
            status: "active".to_string(),
            is_public: false,
            allowed_tools: vec!["custom.tool".to_string()],
            rate_limit_per_minute: 60,
            read_only_role: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn register_contributed_tool(ctx: &AppContext, answer: Value) {
        ctx.hooks.register(
            mcp_events::TOOL_CALL_REQUESTED,
            "test",
            Arc::new(move |context: &mut unified_core::hooks::HookContext| {
                context.add_result(answer.clone());
            }),
        );
    }

    #[tokio::test]
    async fn contributed_tool_without_a_read_only_declaration_is_refused() {
        let ctx = test_context();
        register_contributed_tool(&ctx, json!({ "result": { "ok": true } }));
        let server = test_server();
        let outcome = dispatch_tool(&ctx, &server, "custom.tool", &json!({})).await;
        assert!(outcome.is_err());
        let (code, message, _) = outcome.unwrap_err();
        assert_eq!(code, JSONRPC_SERVER_ERROR);
        assert!(message.contains("read_only"), "{message}");
    }

    #[tokio::test]
    async fn contributed_tool_with_read_only_true_runs_and_is_scrubbed() {
        let ctx = test_context();
        register_contributed_tool(
            &ctx,
            json!({ "result": { "ok": true, "password": "x" }, "read_only": true }),
        );
        let server = test_server();
        let outcome = dispatch_tool(&ctx, &server, "custom.tool", &json!({}))
            .await
            .expect("declared read-only tool runs");
        assert_eq!(outcome["ok"], json!(true));
        assert!(outcome.get("password").is_none());
    }

    #[tokio::test]
    async fn contributed_tool_with_read_only_false_is_refused() {
        let ctx = test_context();
        register_contributed_tool(
            &ctx,
            json!({ "result": { "ok": true }, "read_only": false }),
        );
        let server = test_server();
        let outcome = dispatch_tool(&ctx, &server, "custom.tool", &json!({})).await;
        assert!(outcome.is_err());
        let (code, _, _) = outcome.unwrap_err();
        assert_eq!(code, JSONRPC_SERVER_ERROR);
    }

    #[tokio::test]
    async fn unknown_tool_with_no_handler_is_method_not_found() {
        let ctx = test_context();
        let server = test_server();
        let outcome = dispatch_tool(&ctx, &server, "nope.tool", &json!({})).await;
        assert!(outcome.is_err());
        let (code, message, _) = outcome.unwrap_err();
        assert_eq!(code, JSONRPC_METHOD_NOT_FOUND);
        assert!(message.contains("nope.tool"), "{message}");
    }
}
