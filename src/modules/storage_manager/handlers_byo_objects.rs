// BYO object handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use unified_core::storage::sha256_hex;
use actix_web::{http::header, web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;

use super::*;

// BYO object operations. Every operation re-checks ownership,
// status, the allowed operation, the prefix scope, the file-type
// allowlists, and the quotas - the provider is never reached
// before those checks pass. Keys are relative to the connection
// prefix; the full provider key is always prefix + relative key.

/// Emit a metering delta for one BYO connection with the same
/// `storage.metering.reported` event the plan reports use, so
/// billing sees BYO usage exactly like plan usage.
pub(crate) fn emit_byo_metering(state: &StorageManagerState, connection: &ByoConnection) {
    state.emit(
        events::METERING_REPORTED,
        json!({
            "connection_id": connection.id.to_string(),
            "company_id": connection.company_id,
            "user_id": connection.owner_user,
            "period": Utc::now().format("%Y-%m").to_string(),
            "bytes_stored": connection.used_bytes,
            "files_stored": connection.used_files,
            "source": "byo_usage",
            "reported_at": Utc::now().to_rfc3339(),
        }),
    );
}

pub(crate) async fn list_byo_objects(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
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
    let connection = match load_byo_connection_for(&state, &id, &found.actor, is_admin) {
        Ok(connection) => connection,
        Err(error) => return err_response(error, debug),
    };
    // Listing needs only an active connection that allows reads;
    // the full rule check (prefix, extensions, quotas) applies to
    // the individual object operations below.
    if connection.status != "active" {
        return err_response(
            UnifiedError::conflict(format!(
                "BYO connection \"{}\" is {} and cannot be used",
                connection.name, connection.status
            )),
            debug,
        );
    }
    if !connection.allow_read {
        return err_response(
            UnifiedError::forbidden(format!(
                "BYO connection \"{}\" does not allow read operations",
                connection.name
            )),
            debug,
        );
    }
    let params = params.into_inner();
    let prefix = params.filters.get("prefix").cloned().unwrap_or_default();
    let mut items: Vec<Value> = state
        .byo_objects
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|object| object.connection_id == connection.id)
        .filter(|object| prefix.is_empty() || object.key.starts_with(prefix.as_str()))
        .map(|object| {
            json!({
                "connection_id": object.connection_id,
                "key": object.key,
                "size_bytes": object.size_bytes,
                "mime": object.mime,
                "owner_user": object.owner_user,
                "company_id": object.company_id,
                "recorded": true,
                "created_at": object.created_at,
            })
        })
        .collect();
    let mut listing_supported = false;
    let mut listing_error: Option<String> = None;
    if let Ok(provider) = provider_for_byo(&ctx.config, &connection) {
        let list_prefix = if prefix.is_empty() {
            connection.path_prefix.clone()
        } else {
            format!("{}/{}", connection.path_prefix, prefix)
        };
        match provider.list(&list_prefix).await {
            Ok(keys) => {
                listing_supported = true;
                for key in keys {
                    let relative = key
                        .strip_prefix(&format!("{}/", connection.path_prefix))
                        .unwrap_or(&key)
                        .to_string();
                    if !items.iter().any(|item| {
                        item.get("key").and_then(Value::as_str) == Some(relative.as_str())
                    }) {
                        items.push(json!({
                            "connection_id": connection.id,
                            "key": relative,
                            "size_bytes": Value::Null,
                            "mime": Value::Null,
                            "owner_user": Value::Null,
                            "company_id": Value::Null,
                            "recorded": false,
                            "created_at": Value::Null,
                        }));
                    }
                }
            }
            Err(error) => {
                listing_error = Some(error.message.clone());
            }
        }
    }
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|item| {
            item.get("key")
                .and_then(Value::as_str)
                .is_some_and(|key| key.to_lowercase().contains(&needle))
        });
    }
    items.sort_by(|a, b| {
        a.get("key")
            .and_then(Value::as_str)
            .cmp(&b.get("key").and_then(Value::as_str))
    });
    let page = paginate(items, &params);
    let mut value = page.to_json();
    if let Value::Object(map) = &mut value {
        map.insert(
            "listing_supported".to_string(),
            Value::Bool(listing_supported),
        );
        map.insert(
            "listing_error".to_string(),
            listing_error.map(Value::String).unwrap_or(Value::Null),
        );
    }
    ok(value)
}

pub(crate) async fn upload_byo_object(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    query: web::Query<ByoUploadQuery>,
    body: web::Bytes,
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
    if body.len() > MAX_UPLOAD_BYTES {
        return err_response(
            UnifiedError::bad_request(format!(
                "Upload exceeds the maximum size of {MAX_UPLOAD_BYTES} bytes"
            )),
            debug,
        );
    }
    let mime = query
        .mime
        .clone()
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let size = body.len() as i64;
    let checksum = sha256_hex(&body);
    // The net change this upload makes: an overwrite of an existing
    // key counts only the size delta and zero new files.
    let byo_key = byo_object_key(connection.id, &query.key);
    let previous_size = state
        .byo_objects
        .get(&byo_key)
        .map(|entry| entry.value().size_bytes)
        .unwrap_or(0);
    let is_new = !state.byo_objects.contains_key(&byo_key);
    let delta_bytes = size - previous_size;
    let delta_files = i64::from(is_new);
    let full_key = match check_byo_operation(
        &state,
        &connection,
        "write",
        &query.key,
        Some(&mime),
        delta_bytes,
        delta_files,
    ) {
        Ok(key) => key,
        Err(error) => return err_response(error, debug),
    };
    let provider = match provider_for_byo(&ctx.config, &connection) {
        Ok(provider) => provider,
        Err(error) => return err_response(error, debug),
    };
    // Reserve the connection quota before the provider call so two
    // concurrent uploads cannot both pass against the same counters;
    // the reservation is released again when the upload fails.
    if let Some(mut stored) = state.byo_connections.get_mut(&connection.id) {
        stored.used_bytes = (stored.used_bytes + delta_bytes).max(0);
        stored.used_files = (stored.used_files + delta_files).max(0);
        stored.updated_at = Utc::now();
    }
    let result: UnifiedResult<Value> = (|| async {
        if let Err(error) = provider.upload(&full_key, body.to_vec()).await {
            if let Some(mut stored) = state.byo_connections.get_mut(&connection.id) {
                stored.used_bytes = (stored.used_bytes - delta_bytes).max(0);
                stored.used_files = (stored.used_files - delta_files).max(0);
                stored.updated_at = Utc::now();
            }
            return Err(error);
        }
        let now = Utc::now();
        state.byo_objects.insert(
            byo_object_key(connection.id, &query.key),
            ByoObject {
                connection_id: connection.id,
                key: query.key.clone(),
                size_bytes: size,
                sha256_hex: Some(checksum.clone()),
                mime: mime.clone(),
                owner_user: connection.owner_user.clone(),
                company_id: connection.company_id.clone(),
                created_at: now,
            },
        );
        let updated = load_byo_connection(&state, &id)?;
        state.emit(
            events::OBJECT_STORED,
            json!({
                "connection_id": connection.id.to_string(),
                "key": query.key,
                "bytes": size,
                "sha256_hex": checksum,
                "mime": mime,
                "user_id": found.actor,
                "company_id": connection.company_id,
            }),
        );
        emit_byo_metering(&state, &updated);
        Ok(json!({
            "connection_id": connection.id,
            "key": query.key,
            "size_bytes": size,
            "sha256_hex": checksum,
            "mime": mime,
        }))
    })()
    .await;
    match result {
        Ok(value) => created(value),
        Err(error) => err_response(error, debug),
    }
}

pub(crate) async fn download_byo_object(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    query: web::Query<ObjectQuery>,
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
    let full_key = match check_byo_operation(&state, &connection, "read", &query.key, None, 0, 0) {
        Ok(key) => key,
        Err(error) => return err_response(error, debug),
    };
    let provider = match provider_for_byo(&ctx.config, &connection) {
        Ok(provider) => provider,
        Err(error) => return err_response(error, debug),
    };
    let recorded = state
        .byo_objects
        .get(&byo_object_key(connection.id, &query.key))
        .and_then(|entry| entry.value().sha256_hex.clone());
    let downloaded = match &recorded {
        Some(expected) => provider.download_verified(&full_key, expected).await,
        None => provider.download(&full_key).await,
    };
    match downloaded {
        Ok(bytes) => {
            let mime = state
                .byo_objects
                .get(&byo_object_key(connection.id, &query.key))
                .map(|entry| entry.value().mime.clone())
                .unwrap_or_else(|| "application/octet-stream".to_string());
            let file_name = query
                .key
                .rsplit('/')
                .next()
                .unwrap_or(&query.key)
                .replace('"', "");
            let integrity = if recorded.is_some() {
                "verified"
            } else {
                "unverified"
            };
            HttpResponse::Ok()
                .content_type(mime)
                .insert_header((
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{file_name}\""),
                ))
                .insert_header(("x-object-integrity", integrity))
                .body(bytes)
        }
        Err(error) => {
            if error
                .metadata
                .get("checksum_mismatch")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
            {
                state.emit(
                    events::OBJECT_CHECKSUM_MISMATCH,
                    json!({
                        "connection_id": connection.id.to_string(),
                        "key": query.key,
                        "expected_sha256": recorded,
                    }),
                );
            }
            err_response(error, debug)
        }
    }
}

pub(crate) async fn delete_byo_object(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    query: web::Query<ObjectQuery>,
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
    let full_key = match check_byo_operation(&state, &connection, "delete", &query.key, None, 0, 0) {
        Ok(key) => key,
        Err(error) => return err_response(error, debug),
    };
    let provider = match provider_for_byo(&ctx.config, &connection) {
        Ok(provider) => provider,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<Value> = (|| async {
        provider.delete(&full_key).await?;
        let removed = state
            .byo_objects
            .remove(&byo_object_key(connection.id, &query.key));
        let now = Utc::now();
        if let Some((_, object)) = removed {
            if let Some(mut stored) = state.byo_connections.get_mut(&connection.id) {
                stored.used_bytes = (stored.used_bytes - object.size_bytes).max(0);
                stored.used_files = (stored.used_files - 1).max(0);
                stored.updated_at = now;
            }
        }
        let updated = load_byo_connection(&state, &id)?;
        state.emit(
            events::OBJECT_DELETED,
            json!({
                "connection_id": connection.id.to_string(),
                "key": query.key,
                "user_id": found.actor,
                "company_id": connection.company_id,
            }),
        );
        emit_byo_metering(&state, &updated);
        Ok(json!({
            "deleted": true,
            "connection_id": connection.id,
            "key": query.key,
        }))
    })()
    .await;
    json::respond(result, debug)
}

// Module definition.
