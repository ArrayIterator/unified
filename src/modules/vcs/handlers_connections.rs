// Third-party VCS integrations - Repository connection handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use reqwest::header;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::net::IpAddr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::providers::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_providers::*;
use super::handlers_drafts::*;
use super::webhooks::*;
use super::module::*;

// Connection handlers.

pub(crate) async fn list_connections(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _company = match caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_VCS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let params = params.into_inner();
        let sort = params.allowed_sort(
            &["provider", "owner", "repo", "sync_status", "created_at"],
            "created_at",
        );
        let mut items: Vec<RepositoryConnection> = state
            .connections
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|connection| {
                matches_search(
                    &format!(
                        "{} {}/{} {}",
                        connection.provider.as_str(),
                        connection.owner,
                        connection.repo,
                        connection.software_id
                    ),
                    &params,
                ) && params
                    .filters
                    .get("provider")
                    .is_none_or(|provider| connection.provider.as_str() == provider)
                    && params
                        .filters
                        .get("software_id")
                        .is_none_or(|software_id| connection.software_id == *software_id)
                    && params.filters.get("enabled").is_none_or(|flag| {
                        connection.enabled == (*flag == "true" || *flag == "1")
                    })
                    && params
                        .filters
                        .get("sync_status")
                        .is_none_or(|status| connection.sync_status == *status)
            })
            .collect();
        match sort.as_str() {
            "provider" => items.sort_by(|a, b| a.provider.as_str().cmp(b.provider.as_str())),
            "owner" => items.sort_by(|a, b| a.owner.cmp(&b.owner)),
            "repo" => items.sort_by(|a, b| a.repo.cmp(&b.repo)),
            "sync_status" => items.sort_by(|a, b| a.sync_status.cmp(&b.sync_status)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    body: web::Json<CreateConnection>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_VCS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<RepositoryConnection> = (|| {
        let input = body.into_inner();
        let owner = input.owner.trim().to_string();
        let repo = input.repo.trim().to_string();
        let host = input
            .host
            .map(|host| host.trim().to_string())
            .filter(|host| !host.is_empty());
        let software_id = input.software_id.trim().to_string();
        validate_connection_input(
            input.provider,
            &owner,
            &repo,
            host.as_deref(),
            &software_id,
        )?;
        validate_software_id(&ctx, &software_id)?;
        if state.connections.iter().any(|entry| {
            let connection = entry.value();
            connection.provider == input.provider
                && connection.owner.eq_ignore_ascii_case(&owner)
                && connection.repo.eq_ignore_ascii_case(&repo)
                && connection.host.as_deref() == host.as_deref()
        }) {
            return Err(UnifiedError::conflict(format!(
                "A connection already exists for {}/{} on {}",
                owner,
                repo,
                input.provider.display_name()
            )));
        }
        let connection = RepositoryConnection {
            id: Uuid::now_v7(),
            provider: input.provider,
            owner,
            repo,
            host,
            software_id,
            enabled: true,
            sync_status: "never_synced".to_string(),
            last_synced_at: None,
            last_error: None,
            created_at: now(),
            updated_at: now(),
        };
        state
            .connections
            .insert(connection.id, connection.clone());
        Ok(connection)
    })();
    match result {
        Ok(connection) => json::success_response(connection, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _company = match caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_VCS_VIEW) {
        return json::error_response(&error, debug);
    }
    json::respond(connection_view(&state, id.into_inner()), debug)
}

pub(crate) async fn update_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateConnection>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_VCS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<RepositoryConnection> = (|| {
        let input = body.into_inner();
        let mut connection = state
            .connections
            .get_mut(&id.into_inner())
            .ok_or_else(|| UnifiedError::not_found("Repository connection not found"))?;
        let owner = input
            .owner
            .map(|owner| owner.trim().to_string())
            .unwrap_or_else(|| connection.owner.clone());
        let repo = input
            .repo
            .map(|repo| repo.trim().to_string())
            .unwrap_or_else(|| connection.repo.clone());
        let host = match input.host {
            Some(host) => {
                let host = host.trim().to_string();
                if host.is_empty() {
                    None
                } else {
                    Some(host)
                }
            }
            None => connection.host.clone(),
        };
        let software_id = input
            .software_id
            .as_ref()
            .map(|software_id| software_id.trim().to_string())
            .unwrap_or_else(|| connection.software_id.clone());
        validate_connection_input(
            connection.provider,
            &owner,
            &repo,
            host.as_deref(),
            &software_id,
        )?;
        if let Some(software_id) = &input.software_id {
            validate_software_id(&ctx, software_id.trim())?;
        }
        connection.owner = owner;
        connection.repo = repo;
        connection.host = host;
        connection.software_id = software_id;
        if let Some(enabled) = input.enabled {
            connection.enabled = enabled;
        }
        connection.updated_at = now();
        Ok(connection.clone())
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_VCS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        state
            .connections
            .remove(&id)
            .map(|_| json!({ "deleted": true, "id": id }))
            .ok_or_else(|| UnifiedError::not_found("Repository connection not found"))
    })();
    json::respond(result, debug)
}

pub(crate) fn set_connection_enabled(
    state: &VcsState,
    id: Uuid,
    enabled: bool,
) -> UnifiedResult<RepositoryConnection> {
    let mut connection = state
        .connections
        .get_mut(&id)
        .ok_or_else(|| UnifiedError::not_found("Repository connection not found"))?;
    connection.enabled = enabled;
    connection.updated_at = now();
    Ok(connection.clone())
}

pub(crate) async fn enable_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_VCS_MANAGE) {
        return json::error_response(&error, debug);
    }
    json::respond(set_connection_enabled(&state, id.into_inner(), true), debug)
}

pub(crate) async fn disable_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_VCS_MANAGE) {
        return json::error_response(&error, debug);
    }
    json::respond(
        set_connection_enabled(&state, id.into_inner(), false),
        debug,
    )
}

// Provider releases, import, sync, and sync history.

pub(crate) async fn list_connection_releases(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _company = match caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_VCS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| async {
        let connection = find_connection(&state, id.into_inner())?;
        if !connection.enabled {
            return Err(UnifiedError::conflict(
                "Repository connection is disabled",
            ));
        }
        let pagination = params.normalized();
        let releases = fetch_releases(
            &state,
            &connection,
            pagination.page,
            pagination.per_page,
        )
        .await?;
        let total = releases.items.len() as u64;
        let page = PageResult::new(releases.items, total, &pagination);
        Ok(json!({
            "items": page.items,
            "pagination": page.pagination,
            "rate_limit": releases.rate_limit,
            // The provider paginates independently: `total` counts only
            // this provider page, so callers page through with `page`.
            "note": "pagination.total counts this provider page only; request the next page for more results",
        }))
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn sync_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_VCS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| async {
        let connection = find_connection(&state, id.into_inner())?;
        if !connection.enabled {
            return Err(UnifiedError::conflict(
                "Repository connection is disabled",
            ));
        }
        let started_at = now();
        if let Some(mut entry) = state.connections.get_mut(&connection.id) {
            entry.sync_status = "syncing".to_string();
            entry.updated_at = now();
        }
        match fetch_releases(&state, &connection, 1, 30).await {
            Ok(releases) => {
                let releases_found = releases.items.len() as u32;
                let mut drafts_created: u32 = 0;
                for release in &releases.items {
                    match create_draft(
                        &ctx.hooks,
                        &state,
                        &connection,
                        release,
                        "sync",
                        None,
                        None,
                    ) {
                        Ok(_) => drafts_created += 1,
                        // A draft for this tag already exists: skip it,
                        // it is not a sync failure.
                        Err(error) if error.code == 409 => {}
                        Err(error) => {
                            let message = error.message.clone();
                            if let Some(mut entry) = state.connections.get_mut(&connection.id) {
                                entry.sync_status = "failed".to_string();
                                entry.last_error = Some(message.clone());
                                entry.updated_at = now();
                            }
                            let history = record_sync_history(
                                &state,
                                &connection,
                                "failed",
                                releases_found,
                                drafts_created,
                                Some(message.clone()),
                                releases.rate_limit.remaining,
                                started_at,
                            );
                            ctx.hooks.emit(
                                "vcs.sync.failed",
                                json!({
                                    "connection_id": connection.id,
                                    "provider": connection.provider.as_str(),
                                    "error": message,
                                    "sync_history_id": history.id,
                                }),
                            );
                            return Err(error);
                        }
                    }
                }
                if let Some(mut entry) = state.connections.get_mut(&connection.id) {
                    entry.sync_status = "synced".to_string();
                    entry.last_synced_at = Some(now());
                    entry.last_error = None;
                    entry.updated_at = now();
                }
                let history = record_sync_history(
                    &state,
                    &connection,
                    "completed",
                    releases_found,
                    drafts_created,
                    None,
                    releases.rate_limit.remaining,
                    started_at,
                );
                let connection = find_connection(&state, connection.id)?;
                ctx.hooks.emit(
                    "vcs.sync.completed",
                    json!({
                        "connection_id": connection.id,
                        "provider": connection.provider.as_str(),
                        "releases_found": releases_found,
                        "drafts_created": drafts_created,
                        "sync_history_id": history.id,
                    }),
                );
                Ok(json!({
                    "connection": connection,
                    "releases_found": releases_found,
                    "drafts_created": drafts_created,
                    "sync_history_id": history.id,
                    "rate_limit": releases.rate_limit,
                }))
            }
            Err(error) => {
                let message = error.message.clone();
                if let Some(mut entry) = state.connections.get_mut(&connection.id) {
                    entry.sync_status = "failed".to_string();
                    entry.last_error = Some(message.clone());
                    entry.updated_at = now();
                }
                let history = record_sync_history(
                    &state,
                    &connection,
                    "failed",
                    0,
                    0,
                    Some(message.clone()),
                    None,
                    started_at,
                );
                ctx.hooks.emit(
                    "vcs.sync.failed",
                    json!({
                        "connection_id": connection.id,
                        "provider": connection.provider.as_str(),
                        "error": message,
                        "sync_history_id": history.id,
                    }),
                );
                Err(error)
            }
        }
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn list_connection_sync_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _company = match caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_VCS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let connection_id = id.into_inner();
        find_connection(&state, connection_id)?;
        list_sync_history_filtered(&state, &params.into_inner(), Some(connection_id))
    })();
    json::respond(result, debug)
}
