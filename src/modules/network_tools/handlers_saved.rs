//! HTTP handlers for saved checks.

use unified_core::context::{AppContext};
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Utc};
use serde_json::{json, Value};
use std::sync::{Arc};
use std::time::{Instant};
use uuid::Uuid;
use super::types::{CreateSavedCheckRequest, DEFAULT_TOTAL_TIMEOUT_SECS, NetworkToolSavedCheck, RATE_LIMIT_RUNS_PER_MINUTE, ToolResult, UpdateSavedCheckRequest};
use super::store::{db_delete_saved_check, db_insert_saved_check, db_load_saved_checks, db_pool, db_upsert_saved_check, saved_checks_store};
use super::helpers::{allow_private_probes, check_rate_limit, company_filter, has_capability, matches_search, paginate, parse_company_id, require_capability, require_owner_or_manage, tool_actor, viewer_of};
use super::tools::{tool_dns_lookup, tool_ping, tool_port_check, tool_traceroute};
use super::tools_tls::tool_ssl_check;
use super::tools_net::{tool_blacklist, tool_http_check, tool_ip_info, tool_subnet, tool_webhook_test, tool_whois};
use super::tools_monitor::{execute_tool, run_uptime_probe};

// ---------------------------------------------------------------------------
// Saved checks (CRUD + run).
// ---------------------------------------------------------------------------

pub(crate) const KNOWN_TOOLS: [&str; 13] = [
    "dns",
    "ping",
    "traceroute",
    "port_check",
    "ssl_check",
    "whois",
    "http_check",
    "ip_info",
    "subnet",
    "webhook_test",
    "uptime_check",
    "blacklist",
    "cert_check",
];

pub(crate) fn validate_saved_check(tool: &str, target: &str) -> UnifiedResult<()> {
    if !KNOWN_TOOLS.contains(&tool) {
        return Err(UnifiedError::bad_request(format!(
            "Unknown tool \"{tool}\"; known tools: {}",
            KNOWN_TOOLS.join(", ")
        )));
    }
    if target.trim().is_empty() || target.len() > 2048 {
        return Err(UnifiedError::bad_request(
            "Saved check target must not be empty and must be at most 2048 characters",
        ));
    }
    Ok(())
}

pub(crate) async fn list_saved_checks(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        let company_id = company_filter(&params);
        require_capability(viewer_role.as_deref(), "network_tools:view")?;
        let mut items: Vec<NetworkToolSavedCheck> = match db_pool(&ctx) {
            Some(pool) => db_load_saved_checks(&pool).unwrap_or_else(|_| {
                saved_checks_store()
                    .iter()
                    .map(|entry| entry.value().clone())
                    .collect()
            }),
            None => saved_checks_store()
                .iter()
                .map(|entry| entry.value().clone())
                .collect(),
        };
        if let Some(viewer_id) = &viewer_id {
            items.retain(|check| &check.user_id == viewer_id);
        } else if !has_capability(viewer_role.as_deref(), "network_tools:manage") {
            items.clear();
        }
        if let Some(company) = &company_id {
            items.retain(|check| check.company_id == parse_company_id(Some(company.clone())));
        }
        if let Some(tool) = params.filters.get("tool") {
            items.retain(|check| check.tool == *tool);
        }
        items.retain(|check| matches_search(&format!("{} {}", check.name, check.target), &params));
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_saved_check(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateSavedCheckRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        let company_id = company_filter(&params);
        require_capability(viewer_role.as_deref(), "network_tools:manage")?;
        let input = body.into_inner();
        let name = input.name.trim().to_string();
        if name.is_empty() || name.len() > 128 {
            return Err(UnifiedError::bad_request(
                "Saved check name must be 1-128 characters",
            ));
        }
        validate_saved_check(&input.tool, &input.target)?;
        let now = Utc::now();
        let check = NetworkToolSavedCheck {
            id: Uuid::new_v4(),
            user_id: viewer_id.unwrap_or_else(|| "anonymous".to_string()),
            company_id: parse_company_id(input.company_id.or(company_id)),
            name,
            tool: input.tool,
            target: input.target,
            parameters: input.parameters,
            created_at: now,
            updated_at: now,
        };
        saved_checks_store().insert(check.id, check.clone());
        if let Some(pool) = db_pool(&ctx) {
            let _ = db_insert_saved_check(&pool, &check);
        }
        Ok(json!(check))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_saved_check(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _params = query.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        require_capability(viewer_role.as_deref(), "network_tools:view")?;
        let id = path.into_inner();
        let check = saved_checks_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Saved check {id} not found")))?;
        require_owner_or_manage(viewer_id.as_deref(), viewer_role.as_deref(), &check.user_id)?;
        Ok(json!(check))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_saved_check(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
    body: web::Json<UpdateSavedCheckRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _params = query.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        require_capability(viewer_role.as_deref(), "network_tools:manage")?;
        let id = path.into_inner();
        let mut check = saved_checks_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Saved check {id} not found")))?;
        require_owner_or_manage(viewer_id.as_deref(), viewer_role.as_deref(), &check.user_id)?;
        let input = body.into_inner();
        if let Some(name) = input.name {
            let name = name.trim().to_string();
            if name.is_empty() || name.len() > 128 {
                return Err(UnifiedError::bad_request(
                    "Saved check name must be 1-128 characters",
                ));
            }
            check.name = name;
        }
        if let Some(tool) = input.tool {
            validate_saved_check(&tool, &check.target)?;
            check.tool = tool;
        }
        if let Some(target) = input.target {
            validate_saved_check(&check.tool, &target)?;
            check.target = target;
        }
        if let Some(parameters) = input.parameters {
            check.parameters = parameters;
        }
        check.updated_at = Utc::now();
        saved_checks_store().insert(check.id, check.clone());
        if let Some(pool) = db_pool(&ctx) {
            let _ = db_upsert_saved_check(&pool, &check);
        }
        Ok(json!(check))
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_saved_check(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _params = query.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        require_capability(viewer_role.as_deref(), "network_tools:manage")?;
        let id = path.into_inner();
        let owner = saved_checks_store()
            .get(&id)
            .map(|entry| entry.value().user_id.clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Saved check {id} not found")))?;
        require_owner_or_manage(viewer_id.as_deref(), viewer_role.as_deref(), &owner)?;
        saved_checks_store().remove(&id);
        if let Some(pool) = db_pool(&ctx) {
            let _ = db_delete_saved_check(&pool, id);
        }
        Ok(json!({ "deleted": id }))
    })();
    json::respond(result, debug)
}

/// Re-run a saved check with its stored parameters. The parameter
/// JSON mirrors the per-tool request bodies (for example `{"record_type":
/// "MX"}` for DNS, `{"port": 443, "grab_banner": true}` for a port
/// check); unknown keys are ignored.
pub(crate) async fn run_saved_check(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role) = tool_actor(&req);
    let company_id = company_filter(&params);
    let allow_private = allow_private_probes(viewer_role.as_deref());
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "saved_check:run", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let id = path.into_inner();
    let Some(check) = saved_checks_store()
        .get(&id)
        .map(|entry| entry.value().clone())
    else {
        return json::respond(
            Err::<Value, UnifiedError>(UnifiedError::not_found(format!(
                "Saved check {id} not found"
            ))),
            ctx.is_debug(),
        );
    };
    let timeout_secs = check
        .parameters
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECS);
    let target = check.target.clone();
    let result: UnifiedResult<ToolResult> = match check.tool.as_str() {
        "dns" => {
            let record_type = check
                .parameters
                .get("record_type")
                .and_then(|v| v.as_str())
                .unwrap_or("A")
                .to_string();
            tool_dns_lookup(&check.target, &record_type)
                .await
                .map(|value| ToolResult {
                    tool: "dns".to_string(),
                    target: check.target.clone(),
                    outcome: "success".to_string(),
                    duration_ms: 0,
                    method: Some("dns verifier".to_string()),
                    result: value,
                    notes: Vec::new(),
                })
        }
        "ping" => {
            let count = check
                .parameters
                .get("count")
                .and_then(|v| v.as_u64())
                .unwrap_or(4) as u32;
            tool_ping(&check.target, count, timeout_secs, allow_private).await
        }
        "traceroute" => {
            let max_hops = check
                .parameters
                .get("max_hops")
                .and_then(|v| v.as_u64())
                .unwrap_or(30) as u32;
            tool_traceroute(&check.target, max_hops, timeout_secs, allow_private).await
        }
        "port_check" => {
            let port = check
                .parameters
                .get("port")
                .and_then(|v| v.as_u64())
                .unwrap_or(80) as u16;
            let grab_banner = check
                .parameters
                .get("grab_banner")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            tool_port_check(&check.target, port, grab_banner, timeout_secs, allow_private).await
        }
        "ssl_check" => {
            let port = check
                .parameters
                .get("port")
                .and_then(|v| v.as_u64())
                .unwrap_or(443) as u16;
            tool_ssl_check(&ctx, &check.target, port, timeout_secs, allow_private).await
        }
        "whois" => tool_whois(&check.target, timeout_secs).await,
        "http_check" => {
            let method = check
                .parameters
                .get("method")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let expected = check
                .parameters
                .get("expected_status")
                .and_then(|v| v.as_u64())
                .map(|v| v as u16);
            tool_http_check(&check.target, method, expected, timeout_secs, allow_private).await
        }
        "ip_info" => tool_ip_info(&ctx, &check.target).await,
        "subnet" => {
            let contains = check
                .parameters
                .get("contains")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            tool_subnet(&check.target, contains.as_deref())
        }
        "webhook_test" => {
            let payload = check
                .parameters
                .get("payload")
                .cloned()
                .unwrap_or_else(|| json!({ "event": "network_tools.test", "ok": true }));
            tool_webhook_test(&check.target, payload, timeout_secs, allow_private).await
        }
        "uptime_check" => {
            let expected = check
                .parameters
                .get("expected_status")
                .and_then(|v| v.as_u64())
                .unwrap_or(200) as i32;
            run_uptime_probe(&check.target, expected, timeout_secs, allow_private)
                .await
                .map(|(status, latency_ms, outcome)| ToolResult {
                    tool: "uptime_check".to_string(),
                    target: check.target.clone(),
                    outcome,
                    duration_ms: latency_ms,
                    method: Some("single http probe, no redirect following".to_string()),
                    result: json!({
                        "url": check.target,
                        "status": status,
                        "expected_status": expected,
                        "latency_ms": latency_ms,
                    }),
                    notes: Vec::new(),
                })
        }
        "blacklist" => {
            let zones = check
                .parameters
                .get("zones")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                });
            tool_blacklist(&check.target, zones, timeout_secs).await
        }
        "cert_check" => {
            tool_ssl_check(&ctx, &check.target, 443, timeout_secs, allow_private).await
        }
        other => Err(UnifiedError::bad_request(format!(
            "Saved check names unknown tool \"{other}\""
        ))),
    };
    execute_tool(
        ctx,
        started,
        &actor,
        company_id.as_deref(),
        &check.tool,
        &target,
        result,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_saved_check_accepts_known_tools() {
        for tool in KNOWN_TOOLS {
            assert!(validate_saved_check(tool, "example.com").is_ok(), "tool {tool}");
        }
    }

    #[test]
    fn validate_saved_check_rejects_unknown_tool_and_empty_target() {
        assert!(validate_saved_check("nmap", "example.com").is_err());
        assert!(validate_saved_check("ping", "").is_err());
        assert!(validate_saved_check("ping", "   ").is_err());
    }
}
