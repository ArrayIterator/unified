// BYO connection handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use unified_core::storage::provider_from_config;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::*;

// BYO connections.

pub(crate) async fn list_byo_connections(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_platform_admin(&ctx, &found.role, &req, PERM_MANAGE_BYO);
    let params = params.into_inner();
    let mut items: Vec<ByoConnectionView> = state
        .byo_connections
        .iter()
        .map(|entry| ByoConnectionView::from(entry.value()))
        .filter(|view| is_admin || view.owner_user == found.actor)
        .collect();
    if let Some(filter) = params.filters.get("status") {
        items.retain(|view| view.status == *filter);
    }
    if let Some(filter) = params.filters.get("provider_kind") {
        items.retain(|view| view.provider_kind.as_str() == filter.as_str());
    }
    if let Some(filter) = params.filters.get("company_id") {
        items.retain(|view| view.company_id.as_deref() == Some(filter.as_str()));
    }
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|view| view.name.to_lowercase().contains(&needle));
    }
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    ok(paginate(items, &params))
}

pub(crate) async fn create_byo_connection(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateByoConnectionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<ByoConnectionView> = (|| {
        let input = body.into_inner();
        let settings = state
            .byo_settings
            .read()
            .expect("BYO settings lock poisoned")
            .clone();
        let kind_name = canonical_provider_kind_name(&input.provider_kind)?;
        if !settings.allowed_provider_kinds.contains(&kind_name) {
            return Err(UnifiedError::forbidden(format!(
                "Provider kind \"{kind_name}\" is not on the BYO allowlist (allowed: {})",
                settings.allowed_provider_kinds.join(", ")
            )));
        }
        let kind = storage_kind_for_name(&kind_name)?;
        let s3_compatible = kind_name == "s3-compatible";
        let name = require_text(&input.name, "name", 128)?;
        if state
            .byo_connections
            .iter()
            .any(|entry| entry.value().owner_user == found.actor && entry.value().name == name)
        {
            return Err(UnifiedError::conflict(format!(
                "BYO connection \"{name}\" already exists for this user"
            )));
        }
        if settings.max_connections_per_user > 0 {
            let active_count = state
                .byo_connections
                .iter()
                .filter(|entry| {
                    entry.value().owner_user == found.actor
                        && entry.value().status != "revoked"
                })
                .count() as i64;
            if active_count >= settings.max_connections_per_user {
                return Err(UnifiedError::conflict(format!(
                    "Maximum BYO connections per user ({}) reached",
                    settings.max_connections_per_user
                )));
            }
        }
        let endpoint = optional_text(&input.endpoint, "endpoint", 1024)?.unwrap_or_default();
        if s3_compatible && endpoint.is_empty() {
            return Err(UnifiedError::bad_request(
                "An endpoint is required for the s3-compatible provider kind",
            ));
        }
        let region = optional_text(&input.region, "region", 128)?.unwrap_or_default();
        let container = optional_text(&input.container, "container", 256)?.unwrap_or_default();
        let path = optional_text(&input.path, "path", 1024)?.unwrap_or_default();
        let sftp_host = optional_text(&input.sftp_host, "sftp_host", 253)?.unwrap_or_default();
        let sftp_username =
            optional_text(&input.sftp_username, "sftp_username", 64)?.unwrap_or_default();
        let sftp_auth_kind = match input.sftp_auth_kind {
            None => String::new(),
            Some(raw) => {
                let value = raw.trim().to_string();
                if !value.is_empty() && value != "keyfile" && value != "password" {
                    return Err(UnifiedError::bad_request(
                        "sftp_auth_kind must be \"keyfile\" or \"password\"",
                    ));
                }
                value
            }
        };
        let sftp_keyfile =
            optional_text(&input.sftp_keyfile, "sftp_keyfile", 1024)?.unwrap_or_default();
        let allowed_hosts = input.allowed_hosts.unwrap_or_default();
        for host in &allowed_hosts {
            if host.trim().is_empty() || host.len() > 253 {
                return Err(UnifiedError::bad_request(
                    "allowed_hosts entries must be 1..=253 characters",
                ));
            }
        }
        // Tenant scoping: the connection's company is verified
        // against the caller's scope - a scoped caller connects for
        // their own company (an omitted company adopts it); an
        // unresolved caller may only connect personally.
        let body_company = optional_text(&input.company_id, "company_id", 128)?;
        let company_id = require_company_match_for(&_scope, &body_company)?;
        let path_prefix = match &input.path_prefix {
            Some(raw) => valid_prefix(raw)?,
            None => format!("byo/{}", found.actor),
        };
        if path_prefix.is_empty() {
            return Err(UnifiedError::bad_request(
                "path_prefix must not be empty: a BYO connection is always scoped to a prefix",
            ));
        }
        let max_bytes = valid_quota_limit(input.max_bytes, "max_bytes")?;
        let max_files = valid_quota_limit(input.max_files, "max_files")?;
        let allowed_extensions = validate_extensions(&input.allowed_extensions)?;
        let allowed_mime_types = validate_mime_types(&input.allowed_mime_types)?;
        let (credentials_encrypted, key_fingerprint) = match &input.credentials {
            Some(credentials) => {
                let (encrypted, fingerprint) = encrypt_credentials(&ctx, credentials)?;
                (Some(encrypted), Some(fingerprint))
            }
            None => (None, None),
        };
        let now = Utc::now();
        let connection = ByoConnection {
            id: Uuid::now_v7(),
            name,
            provider_kind: kind,
            s3_compatible,
            endpoint,
            region,
            container,
            path,
            sftp_host,
            sftp_port: input.sftp_port.unwrap_or(0),
            sftp_username,
            sftp_auth_kind,
            sftp_keyfile,
            allowed_hosts,
            credentials_encrypted,
            key_fingerprint,
            path_prefix,
            owner_user: found.actor.clone(),
            company_id,
            status: "draft".to_string(),
            status_reason: None,
            allow_read: input.allow_read.unwrap_or(true),
            allow_write: input.allow_write.unwrap_or(true),
            allow_delete: input.allow_delete.unwrap_or(false),
            max_bytes,
            max_files,
            used_bytes: 0,
            used_files: 0,
            encryption_required: input.encryption_required.unwrap_or(false),
            allowed_extensions,
            allowed_mime_types,
            terms_version: None,
            terms_accepted_at: None,
            last_validated_at: None,
            last_validation_ok: None,
            last_validation_error: None,
            last_health_at: None,
            last_health_ok: None,
            created_at: now,
            updated_at: now,
        };
        // A connection that cannot build a provider cannot be
        // created: fail here, before anything is stored.
        let target = target_config_for_byo(&ctx.config, &connection)?;
        provider_from_config(&target, &connection.allowed_hosts).map(|_| ())?;
        state
            .byo_connections
            .insert(connection.id, connection.clone());
        state.emit(
            events::BYO_CREATED,
            json!({
                "connection_id": connection.id.to_string(),
                "name": connection.name,
                "owner_user": connection.owner_user,
                "company_id": connection.company_id,
                "provider_kind": kind_name,
                "path_prefix": connection.path_prefix,
                "created_by": found.actor,
            }),
        );
        Ok(ByoConnectionView::from(&connection))
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err_response(error, debug),
    }
}

pub(crate) async fn get_byo_connection(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_platform_admin(&ctx, &found.role, &req, PERM_MANAGE_BYO);
    json::respond(
        load_byo_connection_for(&state, &id, &found.actor, is_admin)
            .map(|connection| ByoConnectionView::from(&connection)),
        debug,
    )
}

pub(crate) async fn update_byo_connection(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<UpdateByoConnectionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_platform_admin(&ctx, &found.role, &req, PERM_MANAGE_BYO);
    let result: UnifiedResult<ByoConnectionView> = (|| {
        let input = body.into_inner();
        let mut connection = load_byo_connection_for(&state, &id, &found.actor, is_admin)?;
        if connection.status == "revoked" {
            return Err(UnifiedError::conflict(
                "A revoked BYO connection cannot be updated",
            ));
        }
        if let Some(name) = &input.name {
            let name = require_text(name, "name", 128)?;
            if state.byo_connections.iter().any(|entry| {
                entry.value().id != connection.id
                    && entry.value().owner_user == connection.owner_user
                    && entry.value().name == name
            }) {
                return Err(UnifiedError::conflict(format!(
                    "BYO connection \"{name}\" already exists for this user"
                )));
            }
            connection.name = name;
        }
        if let Some(value) = &input.endpoint {
            connection.endpoint = optional_text(&Some(value.clone()), "endpoint", 1024)?
                .unwrap_or_default();
        }
        if let Some(value) = &input.region {
            connection.region =
                optional_text(&Some(value.clone()), "region", 128)?.unwrap_or_default();
        }
        if let Some(value) = &input.container {
            connection.container =
                optional_text(&Some(value.clone()), "container", 256)?.unwrap_or_default();
        }
        if let Some(value) = &input.path {
            connection.path =
                optional_text(&Some(value.clone()), "path", 1024)?.unwrap_or_default();
        }
        if let Some(value) = &input.sftp_host {
            connection.sftp_host =
                optional_text(&Some(value.clone()), "sftp_host", 253)?.unwrap_or_default();
        }
        if let Some(port) = input.sftp_port {
            connection.sftp_port = port;
        }
        if let Some(value) = &input.sftp_username {
            connection.sftp_username =
                optional_text(&Some(value.clone()), "sftp_username", 64)?.unwrap_or_default();
        }
        if let Some(raw) = &input.sftp_auth_kind {
            let value = raw.trim().to_string();
            if !value.is_empty() && value != "keyfile" && value != "password" {
                return Err(UnifiedError::bad_request(
                    "sftp_auth_kind must be \"keyfile\" or \"password\"",
                ));
            }
            connection.sftp_auth_kind = value;
        }
        if let Some(value) = &input.sftp_keyfile {
            connection.sftp_keyfile =
                optional_text(&Some(value.clone()), "sftp_keyfile", 1024)?.unwrap_or_default();
        }
        if let Some(hosts) = &input.allowed_hosts {
            for host in hosts {
                if host.trim().is_empty() || host.len() > 253 {
                    return Err(UnifiedError::bad_request(
                        "allowed_hosts entries must be 1..=253 characters",
                    ));
                }
            }
            connection.allowed_hosts = hosts.clone();
        }
        // Replacing credentials resets the connection to `draft`:
        // the new credentials must be validated before activation.
        if let Some(credentials) = &input.credentials {
            if credentials.is_empty() {
                connection.credentials_encrypted = None;
                connection.key_fingerprint = None;
            } else {
                let (encrypted, fingerprint) = encrypt_credentials(&ctx, credentials)?;
                connection.credentials_encrypted = Some(encrypted);
                connection.key_fingerprint = Some(fingerprint);
            }
            if connection.status == "active" || connection.status == "validated" {
                connection.status = "draft".to_string();
                connection.status_reason = Some(
                    "Credentials changed: the connection must be validated again before activation"
                        .to_string(),
                );
                connection.last_validation_ok = None;
            }
        }
        if let Some(prefix) = &input.path_prefix {
            let prefix = valid_prefix(prefix)?;
            if prefix.is_empty() {
                return Err(UnifiedError::bad_request(
                    "path_prefix must not be empty: a BYO connection is always scoped to a prefix",
                ));
            }
            connection.path_prefix = prefix;
        }
        if let Some(value) = input.allow_read {
            connection.allow_read = value;
        }
        if let Some(value) = input.allow_write {
            connection.allow_write = value;
        }
        if let Some(value) = input.allow_delete {
            connection.allow_delete = value;
        }
        if let Some(limit) = input.max_bytes {
            connection.max_bytes = valid_quota_limit(limit, "max_bytes")?;
        }
        if let Some(limit) = input.max_files {
            connection.max_files = valid_quota_limit(limit, "max_files")?;
        }
        if let Some(value) = input.encryption_required {
            connection.encryption_required = value;
        }
        if let Some(extensions) = &input.allowed_extensions {
            connection.allowed_extensions = validate_extensions(&Some(extensions.clone()))?;
        }
        if let Some(mime_types) = &input.allowed_mime_types {
            connection.allowed_mime_types = validate_mime_types(&Some(mime_types.clone()))?;
        }
        connection.updated_at = Utc::now();
        let target = target_config_for_byo(&ctx.config, &connection)?;
        provider_from_config(&target, &connection.allowed_hosts).map(|_| ())?;
        state
            .byo_connections
            .insert(connection.id, connection.clone());
        state.emit(
            events::BYO_UPDATED,
            json!({
                "connection_id": connection.id.to_string(),
                "name": connection.name,
                "owner_user": connection.owner_user,
                "status": connection.status,
                "updated_by": found.actor,
            }),
        );
        Ok(ByoConnectionView::from(&connection))
    })();
    json::respond(result, debug)
}

/// Delete a BYO connection. Deleting revokes it first in the audit
/// trail (the connection can no longer move bytes) and removes the
/// recorded objects that belonged to it; the bytes at the provider
/// are left alone, exactly like a bucket delete leaves provider
/// objects alone.
pub(crate) async fn delete_byo_connection(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_platform_admin(&ctx, &found.role, &req, PERM_MANAGE_BYO);
    let result: UnifiedResult<Value> = (|| {
        let connection = load_byo_connection_for(&state, &id, &found.actor, is_admin)?;
        state.byo_connections.remove(&connection.id);
        state
            .byo_objects
            .retain(|_, object| object.connection_id != connection.id);
        state.emit(
            events::BYO_REVOKED,
            json!({
                "connection_id": connection.id.to_string(),
                "name": connection.name,
                "owner_user": connection.owner_user,
                "reason": "deleted",
                "revoked_by": found.actor,
            }),
        );
        Ok(json!({ "deleted": true, "connection_id": connection.id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn validate_byo_connection(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_platform_admin(&ctx, &found.role, &req, PERM_MANAGE_BYO);
    let connection = match load_byo_connection_for(&state, &id, &found.actor, is_admin) {
        Ok(connection) => connection,
        Err(error) => return err_response(error, debug),
    };
    if connection.status == "revoked" {
        return err_response(
            UnifiedError::conflict("A revoked BYO connection cannot be validated"),
            debug,
        );
    }
    let result: UnifiedResult<Value> = (|| async {
        run_byo_validation(&state, &ctx.config, &connection).await?;
        let updated = load_byo_connection(&state, &id)?;
        Ok(json!({
            "connection_id": updated.id,
            "name": updated.name,
            "status": updated.status,
            "ok": true,
            "validated_at": updated.last_validated_at,
        }))
    })()
    .await;
    json::respond(result, debug)
}

/// Activate a BYO connection. Requires, in order: the connection
/// is validated, the provider kind is still allowlisted, the owner
/// has accepted the current terms version (when terms are
/// required), and the module entitlement when the connection is
/// company-scoped.
pub(crate) async fn activate_byo_connection(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_platform_admin(&ctx, &found.role, &req, PERM_MANAGE_BYO);
    let result: UnifiedResult<ByoConnectionView> = (|| {
        let mut connection = load_byo_connection_for(&state, &id, &found.actor, is_admin)?;
        if connection.status == "active" {
            return Ok(ByoConnectionView::from(&connection));
        }
        if connection.status == "revoked" {
            return Err(UnifiedError::conflict(
                "A revoked BYO connection cannot be activated",
            ));
        }
        if !validation_is_fresh(&connection, Utc::now()) {
            return Err(UnifiedError::conflict(
                "BYO connection must pass validation within the last 24 hours before it can activate: run POST /byo/connections/{id}/validate first",
            ));
        }
        let settings = state
            .byo_settings
            .read()
            .expect("BYO settings lock poisoned");
        // Exact canonical-name match: an `s3-compatible` connection
        // needs `s3-compatible` on the allowlist, not just `s3`,
        // and a plain `s3` connection needs `s3`.
        let kind_name = if connection.s3_compatible {
            "s3-compatible".to_string()
        } else {
            connection.provider_kind.as_str().to_string()
        };
        let allowed = settings.allowed_provider_kinds.contains(&kind_name);
        drop(settings);
        if !allowed {
            return Err(UnifiedError::forbidden(format!(
                "Provider kind \"{kind_name}\" is no longer on the BYO allowlist"
            )));
        }
        let terms_required = state
            .byo_settings
            .read()
            .expect("BYO settings lock poisoned")
            .terms_required;
        if terms_required {
            let terms = current_byo_terms(&state).ok_or_else(|| {
                UnifiedError::conflict(
                    "No current BYO terms version is published, so the connection cannot activate",
                )
            })?;
            if !has_accepted_terms(&state, &connection.owner_user, &terms.version) {
                return Err(UnifiedError::conflict(format!(
                    "BYO connection cannot activate until its owner accepts terms version {}",
                    terms.version
                )));
            }
            connection.terms_version = Some(terms.version.clone());
        }
        if let Some(company) = &connection.company_id {
            require_storage_module_entitlement(&ctx, company)?;
        }
        connection.status = "active".to_string();
        connection.status_reason = None;
        connection.updated_at = Utc::now();
        state
            .byo_connections
            .insert(connection.id, connection.clone());
        state.emit(
            events::BYO_ACTIVATED,
            json!({
                "connection_id": connection.id.to_string(),
                "name": connection.name,
                "owner_user": connection.owner_user,
                "company_id": connection.company_id,
                "provider_kind": connection.provider_kind.as_str(),
                "activated_by": found.actor,
            }),
        );
        Ok(ByoConnectionView::from(&connection))
    })();
    json::respond(result, debug)
}

pub(crate) fn set_byo_status(
    state: &StorageManagerState,
    connection: &mut ByoConnection,
    status: &str,
    reason: Option<String>,
    actor: &str,
    event: &str,
) -> ByoConnectionView {
    connection.status = status.to_string();
    connection.status_reason = reason;
    connection.updated_at = Utc::now();
    state
        .byo_connections
        .insert(connection.id, connection.clone());
    state.emit(
        event,
        json!({
            "connection_id": connection.id.to_string(),
            "name": connection.name,
            "owner_user": connection.owner_user,
            "company_id": connection.company_id,
            "status": status,
            "reason": connection.status_reason,
            "changed_by": actor,
        }),
    );
    ByoConnectionView::from(&*connection)
}

pub(crate) async fn suspend_byo_connection(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<ByoStatusRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_platform_admin(&ctx, &found.role, &req, PERM_MANAGE_BYO);
    let result: UnifiedResult<ByoConnectionView> = (|| {
        let input = body.into_inner();
        let mut connection = load_byo_connection_for(&state, &id, &found.actor, is_admin)?;
        if connection.status == "revoked" {
            return Err(UnifiedError::conflict(
                "A revoked BYO connection cannot be suspended",
            ));
        }
        if connection.status == "suspended" {
            return Ok(ByoConnectionView::from(&connection));
        }
        let reason = optional_text(&input.reason, "reason", 512)?;
        Ok(set_byo_status(
            &state,
            &mut connection,
            "suspended",
            reason,
            &found.actor,
            events::BYO_SUSPENDED,
        ))
    })();
    json::respond(result, debug)
}

pub(crate) async fn revoke_byo_connection(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<ByoStatusRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_platform_admin(&ctx, &found.role, &req, PERM_MANAGE_BYO);
    let result: UnifiedResult<ByoConnectionView> = (|| {
        let input = body.into_inner();
        let mut connection = load_byo_connection_for(&state, &id, &found.actor, is_admin)?;
        if connection.status == "revoked" {
            return Ok(ByoConnectionView::from(&connection));
        }
        let reason = optional_text(&input.reason, "reason", 512)?;
        Ok(set_byo_status(
            &state,
            &mut connection,
            "revoked",
            reason,
            &found.actor,
            events::BYO_REVOKED,
        ))
    })();
    json::respond(result, debug)
}
