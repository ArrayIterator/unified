// cloud module — app deployment through the connectors.
//
// A `CloudApp` is a deployed application: a plan (the resource tier)
// plus a Docker or Kubernetes connection plus an image. Deploying
// builds a `CreateServerRequest` from the app and the plan quotas and
// creates the remote resource through the provider adapter (the
// translation layer); the app record tracks the remote resource id
// and its last known status.
//
// Apps are in-memory (DashMap), exactly like hosting plans and
// provider connections. The `cloud_apps` table (migration 000135)
// exists for future persistence; the in-memory store is
// authoritative in this round.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use url::Url;
use uuid::Uuid;

use super::events::{CLOUD_APP_DELETED, CLOUD_APP_DEPLOYED, CLOUD_APP_FAILED};
use super::helpers::{require_cloud_entitlement, PERM_CLOUD_MANAGE, PERM_CLOUD_VIEW};
use super::hosting::types::shared_state;
use super::providers::handlers::{
    caller, find_connection, paginate, require_company, require_enabled, require_permission,
    require_tenant,
};
use super::providers::types::{ContainerPort, CreateServerRequest, ProviderKind};
use super::providers::{provider_registry, ProviderConnection};

// ---------------------------------------------------------------------------
// Types and store.
// ---------------------------------------------------------------------------

/// One deployed application.
#[derive(Debug, Clone, Serialize)]
pub struct CloudApp {
    pub id: Uuid,
    pub name: String,
    pub plan_id: Uuid,
    pub connection_id: Uuid,
    /// `docker` or `kubernetes` (the connection kind).
    pub kind: String,
    /// Remote resource id: a Docker container id, or a Kubernetes
    /// `deployment/<namespace>/<name>`.
    pub resource_id: String,
    pub image: String,
    /// Normalized status: `creating`, `running`, `stopped`, `failed`.
    pub status: String,
    /// Best-effort public URL, from a mapped host port (Docker only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Deploy an app: a plan tier + a Docker/Kubernetes connection +
/// an image, with the container options the adapters understand.
#[derive(Debug, Clone, Deserialize)]
pub struct AppDeployRequest {
    pub name: String,
    pub plan_id: Uuid,
    pub connection_id: Uuid,
    pub image: String,
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
    #[serde(default)]
    pub ports: Option<Vec<ContainerPort>>,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub replicas: Option<u32>,
    #[serde(default)]
    pub namespace: Option<String>,
}

pub(crate) fn apps_store() -> &'static DashMap<Uuid, CloudApp> {
    static STORE: OnceLock<DashMap<Uuid, CloudApp>> = OnceLock::new();
    STORE.get_or_init(DashMap::new)
}

fn find_app(id: Uuid) -> UnifiedResult<CloudApp> {
    apps_store()
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Cloud app \"{id}\" not found")))
}

/// Best-effort public URL for an app: the first mapped host port on
/// the connection endpoint's host. `None` when no host port is
/// mapped or the endpoint has no host.
fn app_url(connection: &ProviderConnection, ports: &Option<Vec<ContainerPort>>) -> Option<String> {
    let host_port = ports
        .as_ref()?
        .iter()
        .find_map(|port| port.host_port)?;
    let endpoint = connection.endpoint.as_deref()?;
    let host = Url::parse(endpoint).ok()?.host_str()?.to_string();
    Some(format!("http://{host}:{host_port}"))
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

pub(crate) async fn create_app(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<AppDeployRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CloudApp> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CLOUD_MANAGE)?;
        require_cloud_entitlement(&ctx, &req)?;
        let name = body.name.trim().to_string();
        if name.is_empty() {
            return Err(UnifiedError::bad_request("App name is required"));
        }
        if body.image.trim().is_empty() {
            return Err(UnifiedError::bad_request("App image is required"));
        }
        // The plan is the resource tier: it must exist and be active,
        // and its quotas bound the container.
        let plan = shared_state()
            .plans
            .get(&body.plan_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Hosting plan \"{}\" not found", body.plan_id))
            })?;
        if !plan.is_active {
            return Err(UnifiedError::bad_request(format!(
                "Hosting plan \"{}\" is not active",
                plan.name
            )));
        }
        let connection = find_connection(body.connection_id)?;
        require_enabled(&connection)?;
        if !matches!(
            connection.kind,
            ProviderKind::Docker | ProviderKind::Kubernetes
        ) {
            return Err(UnifiedError::bad_request(
                "App deploy needs a Docker or Kubernetes connection",
            ));
        }
        let adapter = provider_registry().build(&connection)?;
        if !adapter.capabilities().supports_create {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" does not support create",
                connection.kind.as_str()
            )));
        }
        let request = CreateServerRequest {
            name: name.clone(),
            server_type: None,
            image: Some(body.image.clone()),
            region: None,
            image_id: None,
            plan_id: None,
            root_password: None,
            ssh_key_ids: None,
            namespace: body.namespace.clone(),
            replicas: body.replicas,
            env: body.env.clone(),
            ports: body.ports.clone(),
            command: body.command.clone(),
            memory_mb: Some(plan.ram_mb.max(0) as u64),
            cpu_cores: Some(plan.cpu_cores.max(0.0) as u32),
        };
        let now = Utc::now();
        let mut app = CloudApp {
            id: Uuid::now_v7(),
            name: name.clone(),
            plan_id: plan.id,
            connection_id: connection.id,
            kind: connection.kind.as_str().to_string(),
            resource_id: String::new(),
            image: body.image.clone(),
            status: "creating".to_string(),
            url: app_url(&connection, &body.ports),
            created_at: now,
            updated_at: now,
        };
        let created = match adapter.create_server(&request).await {
            Ok(server) => server,
            Err(error) => {
                ctx.hooks.emit(
                    CLOUD_APP_FAILED,
                    serde_json::json!({
                        "actor": caller.actor.clone(),
                        "connection_id": connection.id,
                        "kind": connection.kind.as_str(),
                        "resource": format!("cloud-app:{}", app.id),
                        "name": app.name,
                        "error": error.message.clone(),
                    }),
                );
                return Err(error);
            }
        };
        app.resource_id = created.id.clone();
        app.status = created.status.clone();
        // Docker create does not start the container; Kubernetes
        // deployments start their pods by themselves.
        if connection.kind == ProviderKind::Docker {
            match adapter.start_server(&created.id).await {
                Ok(started) => {
                    app.status = started.status.clone();
                }
                Err(error) => {
                    // The container exists but would not start: keep
                    // the record as failed so it can be inspected and
                    // cleaned up, and surface the start error.
                    app.status = "failed".to_string();
                    apps_store().insert(app.id, app.clone());
                    ctx.hooks.emit(
                        CLOUD_APP_FAILED,
                        serde_json::json!({
                            "actor": caller.actor.clone(),
                            "connection_id": connection.id,
                            "kind": connection.kind.as_str(),
                            "resource": format!("cloud-app:{}", app.id),
                            "name": app.name,
                            "error": error.message.clone(),
                        }),
                    );
                    return Err(error);
                }
            }
        }
        apps_store().insert(app.id, app.clone());
        ctx.hooks.emit(
            CLOUD_APP_DEPLOYED,
            serde_json::json!({
                "actor": caller.actor.clone(),
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "resource": format!("cloud-app:{}", app.id),
                "name": app.name,
                "resource_id": app.resource_id,
            }),
        );
        Ok(app)
    })()
    .await;
    match result {
        Ok(app) => json::success_response(app, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_apps(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<CloudApp>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CLOUD_VIEW)?;
        require_cloud_entitlement(&ctx, &req)?;
        let mut items: Vec<CloudApp> = apps_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_app(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CloudApp> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CLOUD_VIEW)?;
        require_cloud_entitlement(&ctx, &req)?;
        let mut app = find_app(id.into_inner())?;
        // Refresh the live status, best effort: a failed probe
        // returns the stored record instead of an error.
        if let Ok(connection) = find_connection(app.connection_id) {
            if let Ok(adapter) = provider_registry().build(&connection) {
                if let Ok(live) = adapter.server_status(&app.resource_id).await {
                    if live.status != app.status {
                        app.status = live.status.clone();
                        app.updated_at = Utc::now();
                        apps_store().insert(app.id, app.clone());
                    }
                }
            }
        }
        Ok(app)
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn delete_app(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CloudApp> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CLOUD_MANAGE)?;
        require_cloud_entitlement(&ctx, &req)?;
        let app = find_app(id.into_inner())?;
        // Delete the remote resource; a missing remote resource is
        // not an error (the delete is idempotent), but any other
        // provider failure is.
        if let Ok(connection) = find_connection(app.connection_id) {
            if let Ok(adapter) = provider_registry().build(&connection) {
                match adapter.delete_server(&app.resource_id).await {
                    Ok(()) => {}
                    Err(error) if error.code == 404 => {}
                    Err(error) => return Err(error),
                }
            }
        }
        apps_store().remove(&app.id);
        ctx.hooks.emit(
            CLOUD_APP_DELETED,
            serde_json::json!({
                "actor": caller.actor.clone(),
                "connection_id": app.connection_id,
                "kind": app.kind,
                "resource": format!("cloud-app:{}", app.id),
                "name": app.name,
            }),
        );
        Ok(app)
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn start_app(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CloudApp> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CLOUD_MANAGE)?;
        require_cloud_entitlement(&ctx, &req)?;
        let mut app = find_app(id.into_inner())?;
        let connection = find_connection(app.connection_id)?;
        require_enabled(&connection)?;
        let adapter = provider_registry().build(&connection)?;
        if !adapter.capabilities().supports_start_stop {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" does not support start/stop for apps",
                connection.kind.as_str()
            )));
        }
        let server = adapter.start_server(&app.resource_id).await?;
        app.status = server.status;
        app.updated_at = Utc::now();
        apps_store().insert(app.id, app.clone());
        Ok(app)
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn stop_app(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CloudApp> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CLOUD_MANAGE)?;
        require_cloud_entitlement(&ctx, &req)?;
        let mut app = find_app(id.into_inner())?;
        let connection = find_connection(app.connection_id)?;
        require_enabled(&connection)?;
        let adapter = provider_registry().build(&connection)?;
        if !adapter.capabilities().supports_start_stop {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" does not support start/stop for apps",
                connection.kind.as_str()
            )));
        }
        let server = adapter.stop_server(&app.resource_id).await?;
        app.status = server.status;
        app.updated_at = Utc::now();
        apps_store().insert(app.id, app.clone());
        Ok(app)
    })()
    .await;
    json::respond(result, debug)
}
