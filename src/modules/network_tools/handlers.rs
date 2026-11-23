//! HTTP handlers for running tools and reading run history.

use unified_core::context::{AppContext};
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::{Arc};
use std::time::{Instant};
use uuid::Uuid;
use super::types::{BlacklistRequest, DEFAULT_CONNECT_TIMEOUT_SECS, DEFAULT_DNSBL_ZONES, DEFAULT_TOTAL_TIMEOUT_SECS, DnsLookupRequest, HttpCheckRequest, MAX_PING_COUNT, MAX_TOTAL_TIMEOUT_SECS, MAX_TRACEROUTE_HOPS, NetworkToolRun, PingRequest, PortCheckRequest, RATE_LIMIT_READS_PER_MINUTE, RATE_LIMIT_RUNS_PER_MINUTE, ShareRunRequest, SubnetRequest, ToolResult, ToolTargetRequest, TracerouteRequest, WebhookTestRequest, WhoisRequest, network_tool_events};
use super::store::{db_load_runs, db_pool, runs_store};
use super::helpers::{allow_private_probes, check_rate_limit, company_filter, has_capability, matches_search, paginate, parse_company_id, require_capability, tool_actor, viewer_of};
use super::tools::{tool_dns_lookup, tool_ping, tool_port_check, tool_traceroute};
use super::tools_tls::tool_ssl_check;
use super::tools_net::{tool_blacklist, tool_http_check, tool_ip_info, tool_subnet, tool_webhook_test, tool_whois};
use super::tools_monitor::{execute_tool, run_uptime_probe};

pub(crate) async fn list_tools(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _params = query.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        require_capability(viewer_role.as_deref(), "network_tools:view")?;
        check_rate_limit(
            viewer_id.as_deref().unwrap_or("anonymous"),
            "tools:list",
            RATE_LIMIT_READS_PER_MINUTE,
        )?;
        Ok(json!({
            "tools": [
                { "id": "dns", "name": "DNS lookup", "method": "POST", "path": "/api/v1/network-tools/dns", "record_types": ["A", "AAAA", "MX", "TXT", "CNAME", "NS", "SOA"] },
                { "id": "ping", "name": "Ping", "method": "POST", "path": "/api/v1/network-tools/ping" },
                { "id": "traceroute", "name": "Traceroute", "method": "POST", "path": "/api/v1/network-tools/traceroute" },
                { "id": "port_check", "name": "Port check", "method": "POST", "path": "/api/v1/network-tools/port-check" },
                { "id": "ssl_check", "name": "SSL/TLS checker", "method": "POST", "path": "/api/v1/network-tools/ssl-check" },
                { "id": "whois", "name": "WHOIS lookup", "method": "POST", "path": "/api/v1/network-tools/whois" },
                { "id": "http_check", "name": "HTTP header/status check", "method": "POST", "path": "/api/v1/network-tools/http-check" },
                { "id": "ip_info", "name": "IP info + Geo", "method": "POST", "path": "/api/v1/network-tools/ip-info" },
                { "id": "subnet", "name": "Subnet/CIDR calculator", "method": "POST", "path": "/api/v1/network-tools/subnet" },
                { "id": "webhook_test", "name": "Webhook tester", "method": "POST", "path": "/api/v1/network-tools/webhook-test" },
                { "id": "uptime_check", "name": "Uptime check (one-off)", "method": "POST", "path": "/api/v1/network-tools/uptime-check" },
                { "id": "blacklist", "name": "DNS blacklist check", "method": "POST", "path": "/api/v1/network-tools/blacklist" },
            ],
            "limits": {
                "default_connect_timeout_secs": DEFAULT_CONNECT_TIMEOUT_SECS,
                "default_total_timeout_secs": DEFAULT_TOTAL_TIMEOUT_SECS,
                "max_total_timeout_secs": MAX_TOTAL_TIMEOUT_SECS,
                "max_ping_count": MAX_PING_COUNT,
                "max_traceroute_hops": MAX_TRACEROUTE_HOPS,
                "rate_limit_runs_per_minute": RATE_LIMIT_RUNS_PER_MINUTE,
                "rate_limit_reads_per_minute": RATE_LIMIT_READS_PER_MINUTE,
            },
            "dnsbl_default_zones": DEFAULT_DNSBL_ZONES,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn dns_handler(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<DnsLookupRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role) = tool_actor(&req);
    let company_id = company_filter(&params);
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "dns", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = input.target.clone();
    let result = tool_dns_lookup(&input.target, &input.record_type)
        .await
        .map(|value| ToolResult {
            tool: "dns".to_string(),
            target: input.target.clone(),
            outcome: "success".to_string(),
            duration_ms: 0,
            method: Some("dns verifier".to_string()),
            result: value,
            notes: Vec::new(),
        });
    execute_tool(ctx, started, &actor, company_id.as_deref(), "dns", &target, result).await
}

pub(crate) async fn ping_handler(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<PingRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role) = tool_actor(&req);
    let company_id = company_filter(&params);
    let allow_private = allow_private_probes(viewer_role.as_deref());
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "ping", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = input.target.clone();
    let result = tool_ping(
        &input.target,
        input.count,
        input.timeout_secs.unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECS),
        allow_private,
    )
    .await;
    execute_tool(ctx, started, &actor, company_id.as_deref(), "ping", &target, result).await
}

pub(crate) async fn traceroute_handler(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<TracerouteRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role) = tool_actor(&req);
    let company_id = company_filter(&params);
    let allow_private = allow_private_probes(viewer_role.as_deref());
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "traceroute", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = input.target.clone();
    let result = tool_traceroute(
        &input.target,
        input.max_hops,
        input.timeout_secs.unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECS),
        allow_private,
    )
    .await;
    execute_tool(
        ctx,
        started,
        &actor,
        company_id.as_deref(),
        "traceroute",
        &target,
        result,
    )
    .await
}

pub(crate) async fn port_check_handler(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<PortCheckRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role) = tool_actor(&req);
    let company_id = company_filter(&params);
    let allow_private = allow_private_probes(viewer_role.as_deref());
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "port_check", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = format!("{}:{}", input.target, input.port);
    let result = tool_port_check(
        &input.target,
        input.port,
        input.grab_banner,
        input.timeout_secs.unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECS),
        allow_private,
    )
    .await;
    execute_tool(
        ctx,
        started,
        &actor,
        company_id.as_deref(),
        "port_check",
        &target,
        result,
    )
    .await
}

pub(crate) async fn ssl_check_handler(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<ToolTargetRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role) = tool_actor(&req);
    let company_id = company_filter(&params);
    let allow_private = allow_private_probes(viewer_role.as_deref());
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "ssl_check", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = input.target.clone();
    let result = tool_ssl_check(
        &ctx,
        &input.target,
        443,
        input.timeout_secs.unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECS),
        allow_private,
    )
    .await;
    execute_tool(
        ctx,
        started,
        &actor,
        company_id.as_deref(),
        "ssl_check",
        &target,
        result,
    )
    .await
}

pub(crate) async fn whois_handler(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<WhoisRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role) = tool_actor(&req);
    let company_id = company_filter(&params);
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "whois", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = input.target.clone();
    let result = tool_whois(
        &input.target,
        input.timeout_secs.unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECS),
    )
    .await;
    execute_tool(ctx, started, &actor, company_id.as_deref(), "whois", &target, result).await
}

pub(crate) async fn http_check_handler(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<HttpCheckRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role) = tool_actor(&req);
    let company_id = company_filter(&params);
    let allow_private = allow_private_probes(viewer_role.as_deref());
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "http_check", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = input.url.clone();
    let result = tool_http_check(
        &input.url,
        input.method,
        input.expected_status,
        input.timeout_secs.unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECS),
        allow_private,
    )
    .await;
    execute_tool(
        ctx,
        started,
        &actor,
        company_id.as_deref(),
        "http_check",
        &target,
        result,
    )
    .await
}

pub(crate) async fn ip_info_handler(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<ToolTargetRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role) = tool_actor(&req);
    let company_id = company_filter(&params);
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "ip_info", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = input.target.clone();
    let result = tool_ip_info(&ctx, &input.target).await;
    execute_tool(
        ctx,
        started,
        &actor,
        company_id.as_deref(),
        "ip_info",
        &target,
        result,
    )
    .await
}

pub(crate) async fn subnet_handler(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<SubnetRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role) = tool_actor(&req);
    let company_id = company_filter(&params);
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "subnet", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = input.cidr.clone();
    let result = tool_subnet(&input.cidr, input.contains.as_deref());
    execute_tool(ctx, started, &actor, company_id.as_deref(), "subnet", &target, result).await
}

pub(crate) async fn webhook_test_handler(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<WebhookTestRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role) = tool_actor(&req);
    let company_id = company_filter(&params);
    let allow_private = allow_private_probes(viewer_role.as_deref());
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "webhook_test", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = input.url.clone();
    let result = tool_webhook_test(
        &input.url,
        input.payload,
        input.timeout_secs.unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECS),
        allow_private,
    )
    .await;
    execute_tool(
        ctx,
        started,
        &actor,
        company_id.as_deref(),
        "webhook_test",
        &target,
        result,
    )
    .await
}

pub(crate) async fn blacklist_handler(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<BlacklistRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role) = tool_actor(&req);
    let company_id = company_filter(&params);
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "blacklist", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = input.target.clone();
    let result = tool_blacklist(
        &input.target,
        input.zones,
        input.timeout_secs.unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECS),
    )
    .await;
    execute_tool(
        ctx,
        started,
        &actor,
        company_id.as_deref(),
        "blacklist",
        &target,
        result,
    )
    .await
}

pub(crate) async fn uptime_check_handler(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<HttpCheckRequest>,
) -> HttpResponse {
    let params = query.into_inner();
    let started = Instant::now();
    let (actor, viewer_role) = tool_actor(&req);
    let company_id = company_filter(&params);
    let allow_private = allow_private_probes(viewer_role.as_deref());
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:run")
        .and_then(|_| check_rate_limit(&actor, "uptime_check", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let input = body.into_inner();
    let target = input.url.clone();
    let expected = input.expected_status.unwrap_or(200) as i32;
    let result = run_uptime_probe(
        &input.url,
        expected,
        input.timeout_secs.unwrap_or(DEFAULT_TOTAL_TIMEOUT_SECS),
        allow_private,
    )
    .await
    .map(|(status, latency_ms, outcome)| ToolResult {
        tool: "uptime_check".to_string(),
        target: input.url.clone(),
        outcome,
        duration_ms: latency_ms,
        method: Some("single http probe, no redirect following".to_string()),
        result: json!({
            "url": input.url,
            "status": status,
            "expected_status": expected,
            "latency_ms": latency_ms,
        }),
        notes: Vec::new(),
    });
    execute_tool(
        ctx,
        started,
        &actor,
        company_id.as_deref(),
        "uptime_check",
        &target,
        result,
    )
    .await
}

// ---------------------------------------------------------------------------
// Run history.
// ---------------------------------------------------------------------------

pub(crate) async fn list_history(
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
        check_rate_limit(
            viewer_id.as_deref().unwrap_or("anonymous"),
            "history:list",
            RATE_LIMIT_READS_PER_MINUTE,
        )?;
        let mut items: Vec<NetworkToolRun> = match db_pool(&ctx) {
            Some(pool) => db_load_runs(&pool).unwrap_or_else(|_| {
                runs_store().iter().map(|entry| entry.value().clone()).collect()
            }),
            None => runs_store().iter().map(|entry| entry.value().clone()).collect(),
        };
        // Callers only see their own runs unless they hold manage.
        let can_see_all = has_capability(viewer_role.as_deref(), "network_tools:manage");
        if !can_see_all {
            if let Some(viewer_id) = &viewer_id {
                items.retain(|run| &run.user_id == viewer_id);
            } else {
                items.clear();
            }
        }
        if let Some(company) = &company_id {
            items.retain(|run| run.company_id == parse_company_id(Some(company.clone())));
        }
        if let Some(tool) = params.filters.get("tool") {
            items.retain(|run| run.tool == *tool);
        }
        if let Some(outcome) = params.filters.get("outcome") {
            items.retain(|run| run.outcome == *outcome);
        }
        if let Some(target) = params.filters.get("target") {
            items.retain(|run| run.target.contains(target.as_str()));
        }
        items.retain(|run| matches_search(&format!("{} {}", run.tool, run.target), &params));
        let sort = params.allowed_sort(&["tool", "outcome", "created_at"], "created_at");
        match sort.as_str() {
            "tool" => items.sort_by(|a, b| a.tool.cmp(&b.tool)),
            "outcome" => items.sort_by(|a, b| a.outcome.cmp(&b.outcome)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_history(
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
        let run = runs_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Tool run {id} not found")))?;
        let can_see_all = has_capability(viewer_role.as_deref(), "network_tools:manage");
        if !can_see_all && viewer_id.as_deref() != Some(run.user_id.as_str()) {
            return Err(UnifiedError::forbidden(
                "A run result can only be read by its owner or a manager",
            ));
        }
        Ok(json!(run))
    })();
    json::respond(result, debug)
}

/// Share a run result through the public-link sharing rule.
///
/// The auditors module owns link minting (`POST /api/v1/sharing/links`,
/// scope kind `network_tools_run`, view `summary`). This endpoint
/// verifies ownership, answers the sharing allow-check for the run,
/// and emits `network_tools.result.shared` with the exact scope the
/// caller must mint; the caller then mints the link through the
/// sharing API. The link itself is never minted here, so no token
/// handling is duplicated.
///
/// SEAM for the integrator: register kind `network_tools_run` with
/// view `summary` in the auditors module's `ShareRegistry` (one call
/// in the auditors module setup); until then link creation fails
/// closed with "not a registered shareable kind", by design.
pub(crate) async fn share_history(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
    body: web::Json<ShareRunRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _params = query.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        require_capability(viewer_role.as_deref(), "network_tools:manage")?;
        let id = path.into_inner();
        let run = runs_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Tool run {id} not found")))?;
        let actor = viewer_id.clone().unwrap_or_else(|| "anonymous".to_string());
        if actor != run.user_id
            && !has_capability(viewer_role.as_deref(), "network_tools:probe_private")
            && viewer_role.as_deref() != Some("admin")
            && viewer_role.as_deref() != Some("super_admin")
        {
            return Err(UnifiedError::forbidden(
                "Only the run owner or an admin may share a run result",
            ));
        }
        let expires_in_days = body.into_inner().expires_in_days.unwrap_or(7).clamp(1, 30);
        ctx.hooks.emit(
            network_tool_events::RESULT_SHARED,
            json!({
                "run_id": run.id,
                "tool": run.tool,
                "target": run.target,
                "actor": actor,
                "scope_kind": "network_tools_run",
                "scope_view": "summary",
                "expires_in_days": expires_in_days,
            }),
        );
        Ok(json!({
            "run_id": run.id,
            "scope_kind": "network_tools_run",
            "scope_view": "summary",
            "expires_in_days": expires_in_days,
            "mint_via": "POST /api/v1/sharing/links",
            "mint_body": {
                "scope_kind": "network_tools_run",
                "scope_id": run.id,
                "scope_view": "summary",
                "expires_in_hours": expires_in_days * 24,
            },
            "note": "Mint the link through the sharing API; this module answers sharing.allow_check for the run and never mints tokens itself.",
        }))
    })();
    json::respond(result, debug)
}
