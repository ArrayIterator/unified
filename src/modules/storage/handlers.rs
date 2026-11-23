//! Storage - storage handlers.
//!
//! Part of the `storage` module split; see `src/modules/storage.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use unified_core::json;
use unified_core::pagination::PaginationParams;
use unified_core::storage::StorageKind;
use unified_core::storage::provider_from_config;
use unified_core::storage::sha256_hex;
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use actix_web::http::header;
use actix_web::web;
use chrono::Utc;
use serde_json::Value;
use std::sync::Arc;

use super::helpers::paginate;
use super::helpers::provider_for;
use super::helpers::target_config_from_request;
use super::store::DEFAULT_SIGNED_URL_SECS;
use super::store::MAX_SIGNED_URL_SECS;
use super::store::MAX_UPLOAD_BYTES;
use super::store::PERM_MANAGE_TARGETS;
use super::store::PERM_READ;
use super::store::PERM_WRITE;
use super::store::StoredTarget;
use super::store::authorize;
use super::store::caller_company;
use super::store::object_size_key;
use super::store::object_sizes_store;
use super::store::reconcile_usage_from_listing;
use super::store::registry_target_view;
use super::store::release_usage;
use super::store::require_tenant;
use super::store::target_usage_store;
use super::store::targets_store;
use super::store::try_reserve_usage;
use super::store::upload_lock_for;
use super::types::CreateTargetRequest;
use super::types::ObjectQuery;
use super::types::SignedUrlRequest;
use super::types::StorageStatusView;

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

pub(crate) async fn get_status(ctx: web::Data<Arc<AppContext>>, req: HttpRequest) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<StorageStatusView> = (|| {
        Ok(StorageStatusView {
            kinds: StorageKind::all()
                .iter()
                .map(|kind| kind.as_str().to_string())
                .collect(),
            targets: ctx.storage_registry.list().len(),
            target_names: ctx.storage_registry.list(),
        })
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_targets(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<StoredTarget> = targets_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|target| target.name.to_lowercase().contains(&needle));
        }
        if let Some(kind) = params.filters.get("kind") {
            items.retain(|target| target.kind.as_str() == kind.as_str());
        }
        let sort = params.allowed_sort(&["name", "created_at"], "name");
        match sort.as_str() {
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

pub(crate) async fn create_target(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateTargetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE_TARGETS) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<StoredTarget> = (|| {
        let config = target_config_from_request(&body)?;
        if targets_store().contains_key(&config.name)
            || ctx.storage_registry.get(&config.name).is_some()
        {
            return Err(UnifiedError::conflict(format!(
                "Storage target \"{}\" already exists",
                config.name
            )));
        }
        // Config-declared extra hosts are not reachable from this
        // module without the config section; runtime targets use the
        // built-in provider hosts only. A custom endpoint host must
        // be added to `storage.allowed_hosts` in configuration.
        let provider = provider_from_config(&config, &[])?;
        ctx.storage_registry.register(config.name.clone(), provider);
        let stored = StoredTarget {
            name: config.name.clone(),
            kind: config.kind,
            config: config.masked_config(),
            quota_bytes: config.quota_bytes,
            max_objects: config.max_objects,
            created_at: Utc::now(),
        };
        targets_store().insert(stored.name.clone(), stored.clone());
        ctx.hooks.emit(
            unified_core::storage::events::TARGET_CREATED,
            serde_json::json!({ "target": stored.name, "kind": stored.kind.as_str() }),
        );
        Ok(stored)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_target(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    name: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<StoredTarget> = (|| {
        let name = name.into_inner();
        if let Some(stored) = targets_store().get(&name) {
            return Ok(stored.value().clone());
        }
        // A target declared in configuration lives only in the
        // registry (no stored row): answer from the provider kind
        // instead of 404, like test_target and delete_target do.
        if let Some(provider) = ctx.storage_registry.get(&name) {
            return Ok(registry_target_view(&name, provider.kind()));
        }
        Err(UnifiedError::not_found(format!(
            "Storage target \"{name}\" not found"
        )))
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_target(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    name: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE_TARGETS) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let name = name.into_inner();
        if !targets_store().contains_key(&name) && ctx.storage_registry.get(&name).is_none() {
            return Err(UnifiedError::not_found(format!(
                "Storage target \"{name}\" not found"
            )));
        }
        targets_store().remove(&name);
        // Usage records go with the target so a later target under
        // the same name starts from a clean quota.
        target_usage_store().remove(&name);
        object_sizes_store().retain(|key, _| !key.starts_with(&format!("{name}\n")));
        ctx.storage_registry.remove(&name);
        ctx.hooks.emit(
            unified_core::storage::events::TARGET_DELETED,
            serde_json::json!({ "target": name }),
        );
        Ok(serde_json::json!({ "name": name, "deleted": true }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn test_target(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    name: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE_TARGETS) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| async {
        let name = name.into_inner();
        let provider = provider_for(&ctx, &name)?;
        provider.test_connection().await?;
        let kind = targets_store()
            .get(&name)
            .map(|entry| entry.value().kind.as_str().to_string())
            .unwrap_or_else(|| provider.kind().as_str().to_string());
        Ok(serde_json::json!({ "target": name, "kind": kind, "ok": true }))
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn upload_object(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    name: web::Path<String>,
    query: web::Query<ObjectQuery>,
    body: web::Bytes,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&ctx, &req, PERM_WRITE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| async {
        let name = name.into_inner();
        if body.len() > MAX_UPLOAD_BYTES {
            return Err(UnifiedError::new(
                format!(
                    "Upload exceeds the maximum size of {MAX_UPLOAD_BYTES} bytes"
                ),
                413,
            ));
        }
        // Serialise uploads per target: the quota check, the usage
        // reservation and the size record then see a stable view of
        // the target, so concurrent uploads cannot reserve against
        // the same stale previous size. The size cap above is
        // checked before taking the lock so oversized bodies do
        // not block other uploads.
        let upload_lock = upload_lock_for(&name);
        let _upload_guard = upload_lock.lock().await;
        let size_bytes = body.len() as i64;
        let checksum = sha256_hex(&body);
        // Target-level quota (stored targets only; registry-only
        // targets carry no target-level quota). An overwrite
        // counts only the size delta, and a new object counts one
        // object.
        let size_key = object_size_key(&name, &query.key);
        let previous_size = object_sizes_store()
            .get(&size_key)
            .map(|entry| *entry.value())
            .unwrap_or(0);
        let is_new = !object_sizes_store().contains_key(&size_key);
        let delta_bytes = size_bytes - previous_size;
        let delta_objects = i64::from(is_new);
        let (quota_bytes, max_objects) = match targets_store().get(&name) {
            Some(target) => (target.quota_bytes, target.max_objects),
            None => (None, None),
        };
        let provider = provider_for(&ctx, &name)?;
        // Reserve the delta before uploading, so the usage is
        // counted even if the process crashes after a successful
        // upload; the reservation is rolled back when the upload
        // fails, so a failed upload leaves no usage behind. The
        // check and the reservation happen in one step, so two
        // concurrent uploads cannot both pass against the same
        // pre-upload usage and over-commit the quota.
        try_reserve_usage(&name, delta_bytes, delta_objects, quota_bytes, max_objects)?;
        let stored_key = match provider.upload(&query.key, body.to_vec()).await {
            Ok(stored_key) => stored_key,
            Err(error) => {
                release_usage(&name, delta_bytes, delta_objects);
                return Err(error);
            }
        };
        object_sizes_store().insert(size_key, size_bytes);
        // Usage was already reserved above; there is no second
        // update here to lose to a crash between upload and
        // accounting.
        ctx.hooks.emit(
            unified_core::storage::events::OBJECT_UPLOADED,
            serde_json::json!({ "target": name, "key": query.key, "stored_key": stored_key, "size_bytes": size_bytes, "sha256_hex": checksum }),
        );
        Ok(serde_json::json!({ "target": name, "key": query.key, "stored_key": stored_key, "size_bytes": size_bytes, "sha256_hex": checksum }))
    })()
    .await;
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn download_object(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    name: web::Path<String>,
    query: web::Query<ObjectQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return json::error_response(&error, debug);
    }
    let name = name.into_inner();
    let provider = match provider_for(&ctx, &name) {
        Ok(provider) => provider,
        Err(error) => return json::error_response(&error, debug),
    };
    let downloaded = match &query.expected_sha256 {
        Some(expected) => {
            let expected = expected.trim().to_ascii_lowercase();
            if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
                return json::error_response(
                    &UnifiedError::bad_request(
                        "expected_sha256 must be a 64-character lowercase hex digest",
                    ),
                    debug,
                );
            }
            provider.download_verified(&query.key, &expected).await
        }
        None => provider.download(&query.key).await,
    };
    match downloaded {
        Ok(bytes) => {
            let file_name = query
                .key
                .rsplit('/')
                .next()
                .unwrap_or(&query.key)
                .replace('"', "");
            HttpResponse::Ok()
                .content_type("application/octet-stream")
                .insert_header((
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{file_name}\""),
                ))
                .body(bytes)
        }
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn delete_object(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    name: web::Path<String>,
    query: web::Query<ObjectQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&ctx, &req, PERM_WRITE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| async {
        let name = name.into_inner();
        let provider = provider_for(&ctx, &name)?;
        provider.delete(&query.key).await?;
        let size_key = object_size_key(&name, &query.key);
        if let Some((_, size)) = object_sizes_store().remove(&size_key) {
            if let Some(mut usage) = target_usage_store().get_mut(&name) {
                usage.bytes = (usage.bytes - size).max(0);
                usage.objects = (usage.objects - 1).max(0);
            }
        } else if provider.kind() == StorageKind::Local {
            // No size record: the object was not uploaded through
            // this module, or its record was lost between the size
            // removal and the usage update of an earlier delete, so
            // usage may still count it. The local provider lists
            // its directory recursively, so that listing is
            // authoritative: rebuild usage from the tracked size
            // records of the objects still present, which drops
            // any counted-but-gone contribution. Other providers
            // cannot reconcile here (S3, R2 and Drive do not list
            // at all; SFTP lists only one level), so for them
            // usage keeps tracking module uploads only — see the
            // note on `TargetUsage`.
            if let Ok(keys) = provider.list("").await {
                reconcile_usage_from_listing(&name, &keys);
            }
        }
        ctx.hooks.emit(
            unified_core::storage::events::OBJECT_DELETED,
            serde_json::json!({ "target": name, "key": query.key }),
        );
        Ok(serde_json::json!({ "target": name, "key": query.key, "deleted": true }))
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn signed_url(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    name: web::Path<String>,
    body: web::Json<SignedUrlRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&ctx, &req, PERM_WRITE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| async {
        let name = name.into_inner();
        let expires_secs = body
            .expires_secs
            .unwrap_or(DEFAULT_SIGNED_URL_SECS)
            .clamp(1, MAX_SIGNED_URL_SECS);
        let provider = provider_for(&ctx, &name)?;
        let url = provider.signed_url(&body.key, expires_secs).await?;
        Ok(serde_json::json!({
            "target": name,
            "key": body.key,
            "url": url,
            "expires_secs": expires_secs,
        }))
    })()
    .await;
    json::respond(result, debug)
}
