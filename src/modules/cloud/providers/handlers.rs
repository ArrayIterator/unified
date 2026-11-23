// providers module -- HTTP handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Caller identity and permissions.
// ---------------------------------------------------------------------------

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the supervision and bi modules.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the supervision and bi modules).
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 4, task 316).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before caller identity or any capability
/// check.
///
/// The records in this module carry no company field, so the guard
/// is a fail-closed gate only: real per-record partitioning needs a
/// `tenant_id` on the record (C11 follow-up), not a filter here.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// — a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources.
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified.clone()),
    }
}

/// Fail closed (403) when no company resolves for a mutating
/// handler: the tenant context's company first, the verified
/// caller's server-resolved company otherwise. An unresolved caller
/// writes nothing (the shared `CallerScope::Unresolved` rule) — the
/// records in this module carry no company field to stamp, so there
/// is no global-row write to fall back to.
pub(crate) fn require_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<String> {
    caller_company(tenant, req)?.ok_or_else(|| {
        UnifiedError::forbidden(
            "No company scope resolved for this request: writes require a resolved company",
        )
    })
}

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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

pub(crate) fn find_connection(id: Uuid) -> UnifiedResult<ProviderConnection> {
    connections_store()
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Provider connection \"{id}\" not found")))
}

pub(crate) fn require_enabled(connection: &ProviderConnection) -> UnifiedResult<()> {
    if connection.enabled {
        Ok(())
    } else {
        Err(UnifiedError::conflict(format!(
            "Provider connection \"{}\" is disabled",
            connection.name
        )))
    }
}

pub(crate) fn sort_servers(mut servers: Vec<ServerInfo>, params: &PaginationParams) -> Vec<ServerInfo> {
    let sort = params.allowed_sort(&["name", "status", "created_at"], "name");
    let descending = params.normalized().order == "desc";
    match sort.as_str() {
        "status" => servers.sort_by(|a, b| a.status.cmp(&b.status).then_with(|| a.name.cmp(&b.name))),
        "created_at" => servers.sort_by(|a, b| {
            a.created_at.cmp(&b.created_at).then_with(|| a.name.cmp(&b.name))
        }),
        _ => servers.sort_by(|a, b| a.name.cmp(&b.name)),
    }
    if descending {
        servers.reverse();
    }
    servers
}

pub(crate) fn filter_servers(mut servers: Vec<ServerInfo>, params: &PaginationParams) -> Vec<ServerInfo> {
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        servers.retain(|server| {
            server.name.to_lowercase().contains(&needle)
                || server.id.to_lowercase().contains(&needle)
                || server.status.to_lowercase().contains(&needle)
        });
    }
    if let Some(status) = params.filters.get("status") {
        servers.retain(|server| server.status == *status);
    }
    if let Some(item_kind) = params.filters.get("item_kind") {
        servers.retain(|server| server.item_kind == *item_kind);
    }
    servers
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

/// Catalog of provider kinds and their capabilities.
pub(crate) async fn list_kinds(ctx: web::Data<Arc<AppContext>>, req: HttpRequest) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PROVIDERS_VIEW) {
        return json::error_response(&error, debug);
    }
    if let Err(error) = crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req) {
        return json::error_response(&error, debug);
    }
    let registered = provider_registry().registered_kinds();
    let kinds: Vec<ProviderKind> = ProviderKind::all()
        .iter()
        .copied()
        .filter(|kind| registered.contains(kind))
        .collect();
    let mut views: Vec<ProviderKindView> = kinds
        .iter()
        .map(|kind| ProviderKindView {
            kind: kind.as_str().to_string(),
            display_name: kind.display_name().to_string(),
            capabilities: capabilities_for(*kind),
            allowed_hosts: kind
                .allowed_hosts()
                .iter()
                .map(|host| host.to_string())
                .collect(),
        })
        .collect();
    // Hook-contributed provider definitions (see
    // `cloud::hooks::collect_cloud_provider_definitions`) appear after
    // the built-ins; a contributed id that collides with a built-in
    // is skipped by the collector.
    for definition in crate::modules::cloud::hooks::collect_cloud_provider_definitions(&ctx) {
        if views.iter().any(|view| view.kind == definition.id) {
            continue;
        }
        views.push(ProviderKindView {
            kind: definition.id.clone(),
            display_name: definition.display_name.clone(),
            capabilities: ProviderCapabilities {
                supports_create: definition.capability("create"),
                supports_scale: definition.capability("scale"),
                supports_delete: definition.capability("delete"),
                supports_containers: definition.capability("containers"),
                supports_start_stop: definition.capability("start_stop"),
                supports_suspend: definition.capability("suspend"),
                supports_renew: definition.capability("renew"),
                supports_change_password: definition.capability("change_password"),
                supports_usage: definition.capability("usage"),
            },
            allowed_hosts: definition.allowed_hosts.clone(),
        });
    }
    let result: UnifiedResult<Vec<ProviderKindView>> = Ok(views);
    json::respond(result, debug)
}

pub(crate) async fn create_connection(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateConnectionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProviderConnectionView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROVIDERS_MANAGE)?;
        crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req)?;
        validate_connection(&body)?;
        let kind = ProviderKind::from_str(&body.kind).ok_or_else(|| {
            UnifiedError::bad_request(format!("Unknown provider kind \"{}\"", body.kind))
        })?;
        let now = Utc::now();
        let connection = ProviderConnection {
            id: Uuid::now_v7(),
            name: body.name.trim().to_string(),
            kind,
            endpoint: body
                .endpoint
                .as_ref()
                .map(|endpoint| endpoint.trim().to_string())
                .filter(|endpoint| !endpoint.is_empty()),
            region: body
                .region
                .as_ref()
                .map(|region| region.trim().to_string())
                .filter(|region| !region.is_empty()),
            project_id: body
                .project_id
                .as_ref()
                .map(|project| project.trim().to_string())
                .filter(|project| !project.is_empty()),
            credentials: body.credentials.clone(),
            custom_config: body.custom_config.clone(),
            enabled: body.enabled,
            last_tested_at: None,
            last_status: None,
            created_at: now,
            updated_at: now,
        };
        // Validate the adapter can be built from the stored values
        // before accepting the connection.
        provider_registry().build(&connection)?;
        let view = connection.view();
        connections_store().insert(connection.id, connection.clone());
        ctx.hooks.emit(
            PROVIDER_CONNECTION_CREATED,
            serde_json::json!({
                "actor": caller.actor.clone(),
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "resource": format!("provider-connection:{}", connection.id),
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


pub(crate) async fn list_connections(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROVIDERS_VIEW)?;
        crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req)?;
        let mut items: Vec<ProviderConnectionView> = connections_store()
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


pub(crate) async fn get_connection(ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest, id: web::Path<Uuid>) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PROVIDERS_VIEW) {
        return json::error_response(&error, debug);
    }
    if let Err(error) = crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<ProviderConnectionView> =
        find_connection(id.into_inner()).map(|connection| connection.view());
    json::respond(result, debug)
}

pub(crate) async fn delete_connection(ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROVIDERS_MANAGE)?;
        crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req)?;
        let id = id.into_inner();
        let connection = find_connection(id)?;
        connections_store().remove(&id);
        inventory_store().remove(&id);
        ctx.hooks.emit(
            PROVIDER_CONNECTION_DELETED,
            serde_json::json!({
                "actor": caller.actor.clone(),
                "connection_id": id,
                "kind": connection.kind.as_str(),
                "resource": format!("provider-connection:{id}"),
                "name": connection.name,
            }),
        );
        Ok(serde_json::json!({ "id": id, "deleted": true }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn test_connection(ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProviderInfo> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROVIDERS_MANAGE)?;
        crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req)?;
        let connection = find_connection(id.into_inner())?;
        require_enabled(&connection)?;
        let adapter = provider_registry().build(&connection)?;
        let info = adapter.test_connection().await?;
        if let Some(mut stored) = connections_store().get_mut(&connection.id) {
            stored.last_tested_at = Some(Utc::now());
            stored.last_status = Some("ok".to_string());
            stored.updated_at = Utc::now();
        }
        ctx.hooks.emit(
            PROVIDER_CONNECTED,
            serde_json::json!({
                "actor": caller.actor.clone(),
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "resource": format!("provider-connection:{}", connection.id),
                "latency_ms": info.latency_ms,
            }),
        );
        Ok(info)
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn list_servers(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROVIDERS_VIEW)?;
        crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req)?;
        let connection = find_connection(path.into_inner())?;
        require_enabled(&connection)?;
        let force_refresh = params
            .filters
            .get("refresh")
            .map(|value| value == "true")
            .unwrap_or(false);
        let (servers, cached, fetched_at) = get_inventory(&connection, force_refresh).await?;
        let servers = sort_servers(filter_servers(servers, &params), &params);
        let mut page = paginate(servers, &params).to_json();
        if let Value::Object(map) = &mut page {
            map.insert("cached".to_string(), Value::Bool(cached));
            map.insert(
                "fetched_at".to_string(),
                fetched_at
                    .map(|time| Value::String(time.to_rfc3339()))
                    .unwrap_or(Value::Null),
            );
            map.insert(
                "connection_id".to_string(),
                Value::String(connection.id.to_string()),
            );
        }
        Ok(page)
    })()
    .await;
    json::respond(result, debug)
}


pub(crate) async fn get_inventory_view(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InventoryView> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROVIDERS_VIEW)?;
        crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req)?;
        let connection = find_connection(path.into_inner())?;
        require_enabled(&connection)?;
        let (servers, cached, fetched_at) = get_inventory(&connection, false).await?;
        Ok(InventoryView {
            connection_id: connection.id,
            kind: connection.kind.as_str().to_string(),
            servers,
            cached,
            fetched_at,
            ttl_secs: DEFAULT_INVENTORY_TTL_SECS,
        })
    })()
    .await;
    json::respond(result, debug)
}


pub(crate) async fn get_server_status(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(Uuid, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ServerInfo> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROVIDERS_VIEW)?;
        crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req)?;
        let (connection_id, server_id) = path.into_inner();
        let connection = find_connection(connection_id)?;
        require_enabled(&connection)?;
        let adapter = provider_registry().build(&connection)?;
        adapter.server_status(&server_id).await
    })()
    .await;
    json::respond(result, debug)
}


pub(crate) async fn create_server(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<Uuid>,
    body: web::Json<CreateServerRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ServerInfo> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROVIDERS_MANAGE)?;
        crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req)?;
        let connection = find_connection(path.into_inner())?;
        require_enabled(&connection)?;
        let adapter = provider_registry().build(&connection)?;
        if !adapter.capabilities().supports_create {
            return Err(unsupported(connection.kind, "create", "supports_create"));
        }
        let server = adapter.create_server(&body).await?;
        inventory_store().remove(&connection.id);
        ctx.hooks.emit(
            PROVIDER_SERVER_CREATED,
            serde_json::json!({
                "actor": caller.actor.clone(),
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "resource": format!("provider-server:{}", server.id),
                "server_id": server.id,
                "server_name": server.name,
            }),
        );
        Ok(server)
    })()
    .await;
    match result {
        Ok(server) => json::success_response(server, 201),
        Err(error) => json::error_response(&error, debug),
    }
}


pub(crate) async fn scale_server(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(Uuid, String)>,
    body: web::Json<ScaleServerRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ServerInfo> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROVIDERS_MANAGE)?;
        crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req)?;
        let (connection_id, server_id) = path.into_inner();
        let connection = find_connection(connection_id)?;
        require_enabled(&connection)?;
        let adapter = provider_registry().build(&connection)?;
        if !adapter.capabilities().supports_scale {
            return Err(unsupported(connection.kind, "scale", "supports_scale"));
        }
        let server = adapter.scale_server(&server_id, &body).await?;
        inventory_store().remove(&connection.id);
        ctx.hooks.emit(
            PROVIDER_SERVER_SCALED,
            serde_json::json!({
                "actor": caller.actor.clone(),
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "resource": format!("provider-server:{server_id}"),
                "server_id": server_id,
            }),
        );
        Ok(server)
    })()
    .await;
    json::respond(result, debug)
}


pub(crate) async fn delete_server(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(Uuid, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROVIDERS_MANAGE)?;
        crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req)?;
        let (connection_id, server_id) = path.into_inner();
        let connection = find_connection(connection_id)?;
        require_enabled(&connection)?;
        let adapter = provider_registry().build(&connection)?;
        if !adapter.capabilities().supports_delete {
            return Err(unsupported(connection.kind, "delete", "supports_delete"));
        }
        adapter.delete_server(&server_id).await?;
        inventory_store().remove(&connection.id);
        ctx.hooks.emit(
            PROVIDER_SERVER_DELETED,
            serde_json::json!({
                "actor": caller.actor.clone(),
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "resource": format!("provider-server:{server_id}"),
                "server_id": server_id,
            }),
        );
        Ok(serde_json::json!({ "id": server_id, "deleted": true }))
    })()
    .await;
    json::respond(result, debug)
}


pub(crate) async fn start_server(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(Uuid, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ServerInfo> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROVIDERS_MANAGE)?;
        crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req)?;
        let (connection_id, server_id) = path.into_inner();
        let connection = find_connection(connection_id)?;
        require_enabled(&connection)?;
        let adapter = provider_registry().build(&connection)?;
        if !adapter.capabilities().supports_start_stop {
            return Err(unsupported(
                connection.kind,
                "start",
                "supports_start_stop",
            ));
        }
        let server = adapter.start_server(&server_id).await?;
        inventory_store().remove(&connection.id);
        Ok(server)
    })()
    .await;
    json::respond(result, debug)
}


pub(crate) async fn stop_server(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(Uuid, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ServerInfo> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROVIDERS_MANAGE)?;
        crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req)?;
        let (connection_id, server_id) = path.into_inner();
        let connection = find_connection(connection_id)?;
        require_enabled(&connection)?;
        let adapter = provider_registry().build(&connection)?;
        if !adapter.capabilities().supports_start_stop {
            return Err(unsupported(
                connection.kind,
                "stop",
                "supports_start_stop",
            ));
        }
        let server = adapter.stop_server(&server_id).await?;
        inventory_store().remove(&connection.id);
        Ok(server)
    })()
    .await;
    json::respond(result, debug)
}


pub(crate) async fn suspend_server(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(Uuid, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ServerInfo> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROVIDERS_MANAGE)?;
        crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req)?;
        let (connection_id, server_id) = path.into_inner();
        let connection = find_connection(connection_id)?;
        require_enabled(&connection)?;
        let adapter = provider_registry().build(&connection)?;
        if !adapter.capabilities().supports_suspend {
            return Err(unsupported(
                connection.kind,
                "suspend",
                "supports_suspend",
            ));
        }
        let server = adapter.suspend_server(&server_id).await?;
        inventory_store().remove(&connection.id);
        ctx.hooks.emit(
            PROVIDER_SERVER_SUSPENDED,
            serde_json::json!({
                "actor": caller.actor.clone(),
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "resource": format!("provider-server:{server_id}"),
                "server_id": server_id,
            }),
        );
        Ok(server)
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn unsuspend_server(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(Uuid, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ServerInfo> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROVIDERS_MANAGE)?;
        crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req)?;
        let (connection_id, server_id) = path.into_inner();
        let connection = find_connection(connection_id)?;
        require_enabled(&connection)?;
        let adapter = provider_registry().build(&connection)?;
        if !adapter.capabilities().supports_suspend {
            return Err(unsupported(
                connection.kind,
                "unsuspend",
                "supports_suspend",
            ));
        }
        let server = adapter.unsuspend_server(&server_id).await?;
        inventory_store().remove(&connection.id);
        ctx.hooks.emit(
            PROVIDER_SERVER_UNSUSPENDED,
            serde_json::json!({
                "actor": caller.actor.clone(),
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "resource": format!("provider-server:{server_id}"),
                "server_id": server_id,
            }),
        );
        Ok(server)
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn renew_server(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(Uuid, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ServerInfo> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROVIDERS_MANAGE)?;
        crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req)?;
        let (connection_id, server_id) = path.into_inner();
        let connection = find_connection(connection_id)?;
        require_enabled(&connection)?;
        let adapter = provider_registry().build(&connection)?;
        if !adapter.capabilities().supports_renew {
            return Err(unsupported(
                connection.kind,
                "renew",
                "supports_renew",
            ));
        }
        let server = adapter.renew_server(&server_id).await?;
        inventory_store().remove(&connection.id);
        ctx.hooks.emit(
            PROVIDER_SERVER_RENEWED,
            serde_json::json!({
                "actor": caller.actor.clone(),
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "resource": format!("provider-server:{server_id}"),
                "server_id": server_id,
            }),
        );
        Ok(server)
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn change_server_password(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(Uuid, String)>,
    body: web::Json<ChangePasswordRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ServerInfo> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROVIDERS_MANAGE)?;
        crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req)?;
        let (connection_id, server_id) = path.into_inner();
        let connection = find_connection(connection_id)?;
        require_enabled(&connection)?;
        let adapter = provider_registry().build(&connection)?;
        if !adapter.capabilities().supports_change_password {
            return Err(unsupported(
                connection.kind,
                "change password",
                "supports_change_password",
            ));
        }
        // The password is write-only: it is passed to the adapter and
        // never logged, serialized, or included in any event payload.
        let server = adapter.change_password(&server_id, &body).await?;
        inventory_store().remove(&connection.id);
        ctx.hooks.emit(
            PROVIDER_SERVER_PASSWORD_CHANGED,
            serde_json::json!({
                "actor": caller.actor.clone(),
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "resource": format!("provider-server:{server_id}"),
                "server_id": server_id,
            }),
        );
        Ok(server)
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn get_server_usage(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(Uuid, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<UsageMetrics> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROVIDERS_VIEW)?;
        crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req)?;
        let (connection_id, server_id) = path.into_inner();
        let connection = find_connection(connection_id)?;
        require_enabled(&connection)?;
        let adapter = provider_registry().build(&connection)?;
        if !adapter.capabilities().supports_usage {
            return Err(unsupported(
                connection.kind,
                "usage metrics",
                "supports_usage",
            ));
        }
        adapter.usage_metrics(&server_id).await
    })()
    .await;
    json::respond(result, debug)
}


pub(crate) async fn sync_now(ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest, path: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SyncView> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROVIDERS_MANAGE)?;
        crate::modules::cloud::helpers::require_cloud_entitlement(&ctx, &req)?;
        let connection = find_connection(path.into_inner())?;
        require_enabled(&connection)?;
        let count = sync_connection(&ctx, &connection).await?;
        Ok(SyncView {
            connection_id: connection.id,
            kind: connection.kind.as_str().to_string(),
            server_count: count,
            synced_at: Utc::now(),
        })
    })()
    .await;
    json::respond(result, debug)
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use actix_web::test::TestRequest;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    #[test]
    fn caller_accepts_a_verified_caller() {
        let req = verified_request("user-1", "admin");
        let found = caller(&req).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
    }

    #[test]
    fn caller_rejects_a_request_without_a_verified_caller() {
        let req = TestRequest::default().to_http_request();
        assert!(caller(&req).is_err());
    }

    #[test]
    fn caller_without_a_resolved_role_is_anonymous() {
        // A verified caller with no role resolved holds the anonymous
        // role: the identity resolves, the capability check is what
        // refuses it.
        let req = verified_request_without_roles("user-1");
        let found = caller(&req).expect("a verified caller resolves without a role");
        assert_eq!(found.role, "anonymous");
    }

    #[test]
    fn caller_rejects_requests_without_verified_identity() {
        let req = TestRequest::default().to_http_request();
        assert!(caller(&req).is_err());
    }

    #[test]
    fn permission_constants_are_providers_scoped() {
        assert_eq!(PERM_PROVIDERS_VIEW, "providers.view");
        assert_eq!(PERM_PROVIDERS_MANAGE, "providers.manage");
    }

    #[test]
    fn require_permission_fails_closed_without_a_role_check_handler() {
        // No `role.check` handler is registered on this bus, so the
        // preset `"allowed": false` must stand and the call is refused.
        let ctx = test_context();
        assert!(!check_capability(&ctx, "admin", PERM_PROVIDERS_VIEW));
        assert!(!check_capability(&ctx, "admin", PERM_PROVIDERS_MANAGE));
        assert!(require_permission(&ctx, "admin", PERM_PROVIDERS_VIEW).is_err());
        assert!(require_permission(&ctx, "admin", PERM_PROVIDERS_MANAGE).is_err());
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-1".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(tenant.company_id.as_deref(), Some("company-1"));
        let company = caller_company(&tenant, &req).expect("company must resolve");
        assert_eq!(company.as_deref(), Some("company-1"));
        assert_eq!(
            require_company(&tenant, &req).expect("company must resolve"),
            "company-1"
        );
    }

    #[test]
    fn require_company_fails_closed_without_a_resolved_company() {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: String::new(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(tenant.company_id.is_none());
        assert!(
            caller_company(&tenant, &req)
                .expect("mismatch check must pass")
                .is_none()
        );
        assert!(require_company(&tenant, &req).is_err());
    }

    #[test]
    fn caller_company_refuses_a_verified_company_mismatch() {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-1".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
                .with_company("company-2".to_string()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }
}
