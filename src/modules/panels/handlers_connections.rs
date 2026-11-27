//! panels module - HTTP handlers for panel connections.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

use super::adapter::display_name;
use super::constants::PANEL_CONNECTED;
use super::constants::PANEL_CONNECTION_CREATED;
use super::constants::PANEL_CONNECTION_DELETED;
use super::handlers::CreateConnectionRequest;
use super::handlers::PanelKindView;
use super::handlers::UpdateConnectionRequest;
use super::handlers::authorize_manage;
use super::handlers::authorize_view;
use super::handlers::find_connection;
use super::handlers::paginate;
use super::handlers::require_enabled;
use super::handlers::require_tenant;
use super::registry::panel_registry;
use super::store::connections_store;
use super::store::records_store;
use super::store::sync_state_store;
use super::types::PanelConnection;
use super::types::PanelConnectionView;
use super::types::PanelInfo;
use super::types::PanelKind;
use super::types::capabilities_for;
use super::types::validate_connection;

// Handlers.

/// Catalog of panel kinds and their capabilities.
pub(crate) async fn list_kinds(ctx: web::Data<Arc<AppContext>>, req: HttpRequest) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_tenant(&req) {
        return json::error_response(&error, debug);
    }
    if let Err(error) = authorize_view(&ctx, &req) {
        return json::error_response(&error, debug);
    }
    let registered = panel_registry().registered_kinds();
    let kinds: Vec<PanelKind> = PanelKind::all()
        .iter()
        .copied()
        .filter(|kind| registered.contains(kind))
        .collect();
    let result: UnifiedResult<Vec<PanelKindView>> = Ok(kinds
        .iter()
        .map(|kind| PanelKindView {
            kind: kind.as_str().to_string(),
            display_name: kind.display_name().to_string(),
            capabilities: capabilities_for(*kind),
            required_credentials: kind
                .required_credential_fields()
                .iter()
                .map(|field| field.to_string())
                .collect(),
            endpoint_required: true,
        })
        .collect());
    json::respond(result, debug)
}

pub(crate) async fn create_connection(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateConnectionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PanelConnectionView> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = authorize_manage(&ctx, &req)?;
        let kind = validate_connection(
            &body.kind,
            &body.name,
            body.endpoint.as_deref(),
            &body.credentials,
            body.generic_config.as_ref(),
            body.allow_private_endpoint,
        )?;
        let now = Utc::now();
        let connection = PanelConnection {
            id: Uuid::now_v7(),
            name: body.name.trim().to_string(),
            kind,
            endpoint: body
                .endpoint
                .as_ref()
                .map(|endpoint| endpoint.trim().to_string())
                .filter(|endpoint| !endpoint.is_empty()),
            credentials: body.credentials.clone(),
            generic_config: body.generic_config.clone(),
            allow_private_endpoint: body.allow_private_endpoint,
            enabled: body.enabled,
            last_tested_at: None,
            last_status: None,
            last_synced_at: None,
            created_at: now,
            updated_at: now,
        };
        // Validate the adapter can be built from the stored values
        // before accepting the connection.
        panel_registry().build(&connection)?;
        let view = connection.view();
        connections_store().insert(connection.id, connection.clone());
        ctx.hooks.emit(
            PANEL_CONNECTION_CREATED,
            serde_json::json!({
                "actor": caller.actor,
                "connection_id": connection.id,
                "kind": connection.kind.as_str(),
                "resource": format!("panel-connection:{}", connection.id),
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
    if let Err(error) = require_tenant(&req) {
        return json::error_response(&error, debug);
    }
    if let Err(error) = authorize_view(&ctx, &req) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<PanelConnectionView> = connections_store()
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
            items.retain(|connection| connection.last_status.as_deref() == Some(status.as_str()));
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

pub(crate) async fn get_connection(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_tenant(&req) {
        return json::error_response(&error, debug);
    }
    if let Err(error) = authorize_view(&ctx, &req) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<PanelConnectionView> =
        find_connection(id.into_inner()).map(|connection| connection.view());
    json::respond(result, debug)
}

pub(crate) async fn update_connection(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateConnectionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PanelConnectionView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _ = authorize_manage(&ctx, &req)?;
        let id = id.into_inner();
        let mut connection = find_connection(id)?;
        if let Some(name) = &body.name {
            connection.name = name.trim().to_string();
        }
        if let Some(endpoint) = &body.endpoint {
            connection.endpoint = Some(endpoint.trim().to_string());
        }
        if let Some(credentials) = &body.credentials {
            connection.credentials.merge(credentials);
        }
        if let Some(config) = &body.generic_config {
            connection.generic_config = Some(config.clone());
        }
        if let Some(allow_private) = body.allow_private_endpoint {
            connection.allow_private_endpoint = allow_private;
        }
        if let Some(enabled) = body.enabled {
            connection.enabled = enabled;
        }
        // Re-validate the merged connection before storing it.
        let kind = validate_connection(
            connection.kind.as_str(),
            &connection.name,
            connection.endpoint.as_deref(),
            &connection.credentials,
            connection.generic_config.as_ref(),
            connection.allow_private_endpoint,
        )?;
        connection.kind = kind;
        connection.updated_at = Utc::now();
        panel_registry().build(&connection)?;
        let view = connection.view();
        connections_store().insert(id, connection);
        Ok(view)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_connection(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = authorize_manage(&ctx, &req)?;
        let id = id.into_inner();
        let connection = find_connection(id)?;
        connections_store().remove(&id);
        sync_state_store().remove(&id);
        records_store().retain(|key, _| key.0 != id);
        ctx.hooks.emit(
            PANEL_CONNECTION_DELETED,
            serde_json::json!({
                "actor": caller.actor,
                "connection_id": id,
                "kind": connection.kind.as_str(),
                "resource": format!("panel-connection:{id}"),
                "name": connection.name,
            }),
        );
        Ok(serde_json::json!({ "id": id, "deleted": true }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn test_connection(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PanelInfo> = (|| async {
        let _tenant = require_tenant(&req)?;
        let caller = authorize_manage(&ctx, &req)?;
        let connection = find_connection(id.into_inner())?;
        require_enabled(&connection)?;
        let adapter = panel_registry().build(&connection)?;
        match adapter.test_connection().await {
            Ok(info) => {
                if let Some(mut stored) = connections_store().get_mut(&connection.id) {
                    stored.last_tested_at = Some(Utc::now());
                    stored.last_status = Some("ok".to_string());
                    stored.updated_at = Utc::now();
                }
                ctx.hooks.emit(
                    PANEL_CONNECTED,
                    serde_json::json!({
                        "actor": caller.actor,
                        "connection_id": connection.id,
                        "kind": connection.kind.as_str(),
                        "resource": format!("panel-connection:{}", connection.id),
                        "latency_ms": info.latency_ms,
                    }),
                );
                Ok(info)
            }
            Err(error) => {
                // A failed test must not leave a stale "ok" behind:
                // record the failure on the connection as well.
                if let Some(mut stored) = connections_store().get_mut(&connection.id) {
                    stored.last_tested_at = Some(Utc::now());
                    stored.last_status = Some("error".to_string());
                    stored.updated_at = Utc::now();
                }
                Err(error)
            }
        }
    })()
    .await;
    json::respond(result, debug)
}
