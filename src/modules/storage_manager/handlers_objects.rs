// Object handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use unified_core::storage::{sha256_hex, validate_key};
use actix_web::{http::header, web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use super::*;

// ---------------------------------------------------------------------------
// Objects (file browser + upload / download / delete / move).
// ---------------------------------------------------------------------------

pub(crate) async fn list_objects(
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
    let found = match authorize(&ctx, &req, PERM_BROWSE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_storage_admin(&ctx, &found.role);
    let bucket = match load_bucket(&state, &id) {
        Ok(bucket) => bucket,
        Err(error) => return err_response(error, debug),
    };
    // Tenant scoping: a non-admin may list only a bucket of their
    // own company (or a shared, company-less bucket — where the
    // object filter below still applies).
    if let Err(error) = check_bucket_access(&bucket, _scope.as_deref(), is_admin) {
        return err_response(error, debug);
    }
    let params = params.into_inner();
    let prefix = params
        .filters
        .get("prefix")
        .cloned()
        .unwrap_or_default();
    let mut items: Vec<ObjectView> = state
        .objects
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|object| object.bucket_id == bucket.id)
        .filter(|object| {
            // Object-level scoping: a non-admin sees only objects
            // they own or that belong to their company.
            is_admin
                || object.owner_user == found.actor
                || _scope
                    .as_deref()
                    .is_some_and(|company| object.company_id.as_deref() == Some(company))
        })
        .filter(|object| prefix.is_empty() || object.key.starts_with(prefix.as_str()))
        .map(|object| ObjectView {
            bucket_id: object.bucket_id,
            key: object.key.clone(),
            size_bytes: Some(object.size_bytes),
            sha256_hex: object.sha256_hex.clone(),
            mime: Some(object.mime.clone()),
            owner_module: Some(object.owner_module.clone()),
            owner_user: Some(object.owner_user.clone()),
            company_id: object.company_id.clone(),
            archived: object.archived,
            recorded: true,
            created_at: Some(object.created_at),
        })
        .collect();
    // Live enumeration where the provider supports it: unrecorded
    // keys are shown with `recorded: false`. When the provider
    // cannot list, the recorded rows are the whole answer (see the
    // module header) — that is reported, not hidden.
    let mut listing_supported = false;
    let mut listing_error: Option<String> = None;
    if let Ok(provider) = provider_for_bucket(&ctx, &bucket) {
        match provider.list(&prefix).await {
            Ok(keys) => {
                listing_supported = true;
                // Live-only keys have unknown ownership, so they
                // are shown to admins only — a non-admin must never
                // learn about (or fetch) an object whose owner the
                // hub cannot prove.
                if is_admin {
                    for key in keys {
                        if !items.iter().any(|item| item.key == key) {
                            items.push(ObjectView {
                                bucket_id: bucket.id,
                                key,
                                size_bytes: None,
                                sha256_hex: None,
                                mime: None,
                                owner_module: None,
                                owner_user: None,
                                company_id: None,
                                archived: false,
                                recorded: false,
                                created_at: None,
                            });
                        }
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
        items.retain(|object| object.key.to_lowercase().contains(&needle));
    }
    if let Some(module) = params.filters.get("module") {
        items.retain(|object| object.owner_module.as_deref() == Some(module.as_str()));
    }
    if let Some(user) = params.filters.get("user_id") {
        items.retain(|object| object.owner_user.as_deref() == Some(user.as_str()));
    }
    items.sort_by(|a, b| a.key.cmp(&b.key));
    if params.normalized().order == "desc" {
        items.reverse();
    }
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

pub(crate) async fn upload_object(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    query: web::Query<UploadQuery>,
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
    let found = match authorize(&ctx, &req, PERM_UPLOAD) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_storage_admin(&ctx, &found.role);
    let result: UnifiedResult<Value> = (|| async {
        let bucket = load_bucket(&state, &id)?;
        // Tenant scoping: a non-admin uploads only into a bucket
        // of their own company (or a shared bucket), and the
        // recorded company is their own company — never a
        // caller-supplied ?company_id= for another tenant.
        check_bucket_access(&bucket, _scope.as_deref(), is_admin)?;
        let company_id = resolve_upload_company(
            query.company_id.as_deref(),
            _scope.as_deref(),
            is_admin,
        )?;
        if body.len() > MAX_UPLOAD_BYTES {
            return Err(UnifiedError::new(
                format!("Upload exceeds the maximum size of {MAX_UPLOAD_BYTES} bytes"),
                413,
            ));
        }
        let key = query.key.trim().to_string();
        validate_key(&key)?;
        let module_id = match &query.module {
            Some(raw) => valid_module_id(raw)?,
            None => "files".to_string(),
        };
        let mime = query
            .mime
            .clone()
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let size_bytes = body.len() as i64;
        let checksum = sha256_hex(&body);
        // The net change this upload makes: an overwrite of an
        // existing key counts only the size delta and zero new
        // files, so re-uploading the same key cannot double-count.
        let size_key = object_key(bucket.id, &key);
        // Check + reserve under one guard so two concurrent uploads
        // cannot both pass against the same counters (the guard is
        // only held across synchronous work, never the provider
        // call). The reservation is released again when the provider
        // upload fails. The previous size / new-key read happens
        // INSIDE the guard: two concurrent uploads of the same new
        // key must not both see `is_new == true` and both reserve
        // a file-count delta.
        let (check, delta_bytes, delta_files) = {
            let _guard = state
                .quota_guard
                .lock()
                .expect("quota guard lock poisoned");
            let (delta_bytes, delta_files) = upload_delta(
                state
                    .objects
                    .get(&size_key)
                    .map(|entry| entry.value().size_bytes),
                size_bytes,
            );
            let check = check_quotas(
                &state,
                &module_id,
                &found.actor,
                company_id.as_deref(),
                Some(bucket.provider_kind.as_str()),
                delta_bytes,
                delta_files,
            );
            if !check.exceeded {
                reserve_quota(
                    &state,
                    &module_id,
                    &found.actor,
                    company_id.as_deref(),
                    delta_bytes,
                    delta_files,
                );
            }
            (check, delta_bytes, delta_files)
        };
        if check.exceeded {
            state.emit(
                events::QUOTA_EXCEEDED,
                json!({
                    "scope": check.scope,
                    "scope_id": check.scope_id,
                    "module": module_id,
                    "user_id": found.actor,
                    "company_id": company_id,
                    "bucket_id": bucket.id.to_string(),
                    "key": key,
                    "bytes": body.len(),
                    "detail": check.detail,
                }),
            );
            return Err(UnifiedError::conflict(check.detail)
                .with_metadata("quota_exceeded", json!(true))
                .with_metadata("scope", json!(check.scope))
                .with_metadata("scope_id", json!(check.scope_id)));
        }
        let provider = provider_for_bucket(&ctx, &bucket)?;
        let stored_key = match provider.upload(&key, body.to_vec()).await {
            Ok(stored_key) => stored_key,
            Err(error) => {
                release_quota(
                    &state,
                    &module_id,
                    &found.actor,
                    company_id.as_deref(),
                    delta_bytes,
                    delta_files,
                );
                return Err(error);
            }
        };
        record_stored(
            &state,
            bucket.id,
            &stored_key,
            size_bytes,
            Some(checksum.clone()),
            &mime,
            &module_id,
            &found.actor,
            company_id.as_deref(),
            false,
        );
        // The object row now counts towards the plan usage, so the
        // plan part of the reservation is released (the explicit
        // quota counters stay reserved — they were the reservation).
        release_plan_reservation(
            &state,
            company_id.as_deref(),
            &found.actor,
            delta_bytes,
            delta_files,
        );
        if check.warning {
            state.emit(
                events::QUOTA_WARNING,
                json!({
                    "scope": check.scope,
                    "scope_id": check.scope_id,
                    "module": module_id,
                    "user_id": found.actor,
                    "company_id": company_id,
                    "bucket_id": bucket.id.to_string(),
                    "key": stored_key,
                    "bytes": body.len(),
                    "detail": check.detail,
                }),
            );
        }
        state.emit(
            events::OBJECT_STORED,
            json!({
                "bucket_id": bucket.id.to_string(),
                "bucket_name": bucket.name,
                "key": stored_key,
                "size_bytes": body.len(),
                "sha256_hex": checksum,
                "mime": mime,
                "module": module_id,
                "owner_user": found.actor,
                "company_id": company_id,
            }),
        );
        Ok(json!({
            "bucket_id": bucket.id,
            "key": stored_key,
            "size_bytes": body.len(),
            "sha256_hex": checksum,
            "mime": mime,
            "module": module_id,
            "quota_warning": check.warning,
            "quota_warning_detail": if check.warning { Some(check.detail) } else { None },
        }))
    })()
    .await;
    match result {
        Ok(view) => created(view),
        Err(error) => err_response(error, debug),
    }
}

pub(crate) async fn download_object(
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
    let found = match authorize(&ctx, &req, PERM_BROWSE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_storage_admin(&ctx, &found.role);
    let bucket = match load_bucket(&state, &id) {
        Ok(bucket) => bucket,
        Err(error) => return err_response(error, debug),
    };
    if let Err(error) = check_bucket_access(&bucket, _scope.as_deref(), is_admin) {
        return err_response(error, debug);
    }
    if let Err(error) = validate_key(&query.key) {
        return err_response(error, debug);
    }
    // Object-level scoping: a non-admin downloads only objects
    // they own or that belong to their company. An unrecorded key
    // has unknown ownership and is refused for non-admins.
    match state.objects.get(&object_key(bucket.id, &query.key)) {
        Some(entry) => {
            if let Err(error) =
                check_object_access(entry.value(), &found.actor, _scope.as_deref(), is_admin)
            {
                return err_response(error, debug);
            }
        }
        None if !is_admin => {
            return err_response(
                UnifiedError::not_found(format!("Storage object not found: {}", query.key)),
                debug,
            );
        }
        None => {}
    }
    let provider = match provider_for_bucket(&ctx, &bucket) {
        Ok(provider) => provider,
        Err(error) => return err_response(error, debug),
    };
    let recorded = state
        .objects
        .get(&object_key(bucket.id, &query.key))
        .and_then(|entry| entry.value().sha256_hex.clone());
    let downloaded = match &recorded {
        Some(expected) => provider.download_verified(&query.key, expected).await,
        None => provider.download(&query.key).await,
    };
    match downloaded {
        Ok(bytes) => {
            let mime = state
                .objects
                .get(&object_key(bucket.id, &query.key))
                .map(|entry| entry.value().mime.clone())
                .unwrap_or_else(|| "application/octet-stream".to_string());
            let file_name = query
                .key
                .rsplit('/')
                .next()
                .unwrap_or(&query.key)
                .replace('"', "");
            // `verified` when the bytes matched a recorded checksum,
            // `unverified` for objects recorded before checksums were
            // kept (see the verify endpoint to backfill them).
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
                        "bucket_id": bucket.id.to_string(),
                        "key": query.key,
                        "expected_sha256": recorded,
                    }),
                );
            }
            err_response(error, debug)
        }
    }
}

pub(crate) async fn delete_object(
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
    let found = match authorize(&ctx, &req, PERM_UPLOAD) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_storage_admin(&ctx, &found.role);
    let result: UnifiedResult<Value> = (|| async {
        let bucket = load_bucket(&state, &id)?;
        check_bucket_access(&bucket, _scope.as_deref(), is_admin)?;
        validate_key(&query.key)?;
        // Only the owner (or their company, or an admin) may
        // delete a recorded object; an unrecorded key is refused
        // for non-admins (unknown ownership).
        match state.objects.get(&object_key(bucket.id, &query.key)) {
            Some(entry) => {
                check_object_access(entry.value(), &found.actor, _scope.as_deref(), is_admin)?;
            }
            None if !is_admin => {
                return Err(UnifiedError::not_found(format!(
                    "Storage object not found: {}",
                    query.key
                )));
            }
            None => {}
        }
        let provider = provider_for_bucket(&ctx, &bucket)?;
        provider.delete(&query.key).await?;
        record_deleted(&state, bucket.id, &query.key);
        state.emit(
            events::OBJECT_DELETED,
            json!({
                "bucket_id": bucket.id.to_string(),
                "bucket_name": bucket.name,
                "key": query.key,
                "deleted_by": found.actor,
            }),
        );
        Ok(json!({ "bucket_id": bucket.id, "key": query.key, "deleted": true }))
    })()
    .await;
    json::respond(result, debug)
}

/// Verify one recorded object: download it, hash the bytes, and
/// compare with the recorded SHA-256. A mismatch is reported as
/// data (`"status": "mismatch"`) and emits
/// `storage.object.checksum_mismatch` — the endpoint itself never
/// fails on a mismatch, so a scanner can sweep a whole bucket. An
/// object with no recorded checksum is backfilled from the bytes
/// just read (`"status": "recorded"`), so later downloads verify.
/// A provider-level checksum failure (local / SFTP sidecar) is
/// reported as a mismatch too.
pub(crate) async fn verify_object(
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
    let found = match authorize(&ctx, &req, PERM_BROWSE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_storage_admin(&ctx, &found.role);
    let result: UnifiedResult<Value> = (|| async {
        let bucket = load_bucket(&state, &id)?;
        check_bucket_access(&bucket, _scope.as_deref(), is_admin)?;
        validate_key(&query.key)?;
        match state.objects.get(&object_key(bucket.id, &query.key)) {
            Some(entry) => {
                check_object_access(entry.value(), &found.actor, _scope.as_deref(), is_admin)?;
            }
            None if !is_admin => {
                return Err(UnifiedError::not_found(format!(
                    "Storage object not found: {}",
                    query.key
                )));
            }
            None => {}
        }
        let provider = provider_for_bucket(&ctx, &bucket)?;
        let recorded = state
            .objects
            .get(&object_key(bucket.id, &query.key))
            .and_then(|entry| entry.value().sha256_hex.clone());
        match provider.download(&query.key).await {
            Ok(bytes) => {
                let actual = sha256_hex(&bytes);
                match &recorded {
                    Some(expected) if *expected == actual => Ok(json!({
                        "bucket_id": bucket.id,
                        "key": query.key,
                        "recorded_sha256": expected,
                        "actual_sha256": actual,
                        "match": true,
                        "status": "verified",
                    })),
                    Some(expected) => {
                        state.emit(
                            events::OBJECT_CHECKSUM_MISMATCH,
                            json!({
                                "bucket_id": bucket.id.to_string(),
                                "key": query.key,
                                "expected_sha256": expected,
                                "actual_sha256": actual,
                            }),
                        );
                        Ok(json!({
                            "bucket_id": bucket.id,
                            "key": query.key,
                            "recorded_sha256": expected,
                            "actual_sha256": actual,
                            "match": false,
                            "status": "mismatch",
                        }))
                    }
                    None => {
                        if let Some(mut object) = state
                            .objects
                            .get_mut(&object_key(bucket.id, &query.key))
                        {
                            object.sha256_hex = Some(actual.clone());
                            object.updated_at = Utc::now();
                        }
                        Ok(json!({
                            "bucket_id": bucket.id,
                            "key": query.key,
                            "recorded_sha256": Value::Null,
                            "actual_sha256": actual,
                            "match": Value::Null,
                            "status": "recorded",
                        }))
                    }
                }
            }
            Err(error)
                if error
                    .metadata
                    .get("checksum_mismatch")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false) =>
            {
                state.emit(
                    events::OBJECT_CHECKSUM_MISMATCH,
                    json!({
                        "bucket_id": bucket.id.to_string(),
                        "key": query.key,
                        "expected_sha256": recorded,
                    }),
                );
                Ok(json!({
                    "bucket_id": bucket.id,
                    "key": query.key,
                    "recorded_sha256": recorded,
                    "actual_sha256": Value::Null,
                    "match": false,
                    "status": "mismatch",
                }))
            }
            Err(error) => Err(error),
        }
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub(crate) struct OrphanCleanupRequest {
    /// When true (the default) only report the orphans; pass
    /// `false` to actually delete them.
    #[serde(default = "default_true")]
    pub(crate) dry_run: bool,
    /// Only scan keys under this prefix (empty means the whole
    /// bucket).
    #[serde(default)]
    pub(crate) prefix: String,
}

/// Delete provider objects that have no recorded hub row
/// (orphans): objects uploaded outside the hub, or left behind by
/// a failed or partial operation. Dry-run by default — the caller
/// must pass `"dry_run": false` to delete anything. Providers that
/// cannot enumerate objects (S3 / R2 / Drive) answer with an error
/// instead of pretending the bucket is empty.
pub(crate) async fn cleanup_orphans(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<OrphanCleanupRequest>,
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
    let found = match authorize(&ctx, &req, PERM_MANAGE_BUCKETS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_storage_admin(&ctx, &found.role);
    let result: UnifiedResult<Value> = (|| async {
        let bucket = load_bucket(&state, &id)?;
        check_bucket_access(&bucket, _scope.as_deref(), is_admin)?;
        let prefix = body.prefix.trim().to_string();
        let provider = provider_for_bucket(&ctx, &bucket)?;
        let keys = provider.list(&prefix).await.map_err(|error| {
            UnifiedError::bad_request(format!(
                "Orphan scan needs a provider listing: {}",
                error.message
            ))
        })?;
        if keys.len() > 10_000 {
            return Err(UnifiedError::bad_request(
                "Orphan scan found more than 10000 keys; narrow it with a prefix",
            ));
        }
        let mut orphans: Vec<String> = Vec::new();
        for key in &keys {
            if !state
                .objects
                .contains_key(&object_key(bucket.id, key))
            {
                orphans.push(key.clone());
            }
        }
        let orphan_count = orphans.len();
        let mut deleted = 0usize;
        let mut errors: Vec<String> = Vec::new();
        if !body.dry_run {
            for key in &orphans {
                match provider.delete(key).await {
                    Ok(()) => deleted += 1,
                    Err(error) => {
                        if errors.len() < 10 {
                            errors.push(format!("\"{key}\": {}", error.message));
                        }
                    }
                }
            }
        }
        Ok(json!({
            "bucket_id": bucket.id,
            "prefix": prefix,
            "dry_run": body.dry_run,
            "examined": keys.len(),
            "orphan_count": orphan_count,
            "orphans": orphans.iter().take(100).cloned().collect::<Vec<_>>(),
            "deleted": deleted,
            "errors": errors,
        }))
    })()
    .await;
    json::respond(result, debug)
}

/// Refuse a move whose destination key already exists, unless the
/// caller explicitly passed `overwrite: true`. A silent clobber
/// would destroy the destination object without warning.
pub(crate) fn move_destination_check(
    destination_exists: bool,
    overwrite: bool,
    new_key: &str,
) -> UnifiedResult<()> {
    if destination_exists && !overwrite {
        return Err(UnifiedError::conflict(format!(
            "Destination object \"{new_key}\" already exists; pass overwrite: true to replace it"
        )));
    }
    Ok(())
}

/// Net quota delta of a move: the source and destination hold the
/// same bytes, so a move onto a fresh key changes nothing net
/// (0 bytes, 0 files); a move that overwrites an existing
/// destination frees the difference between the two sizes.
pub(crate) fn move_quota_delta(source_size: i64, destination_size: Option<i64>) -> (i64, i64) {
    match destination_size {
        Some(old) => (source_size - old, 0),
        None => (0, 0),
    }
}

pub(crate) async fn move_object(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<MoveObjectRequest>,
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
    let found = match authorize(&ctx, &req, PERM_UPLOAD) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_storage_admin(&ctx, &found.role);
    let result: UnifiedResult<Value> = (|| async {
        let bucket = load_bucket(&state, &id)?;
        check_bucket_access(&bucket, _scope.as_deref(), is_admin)?;
        let input = body.into_inner();
        validate_key(&input.key)?;
        validate_key(&input.new_key)?;
        if input.key == input.new_key {
            return Err(UnifiedError::bad_request(
                "new_key must differ from key",
            ));
        }
        // Only the owner (or their company, or an admin) may move
        // a recorded object; an unrecorded source is refused for
        // non-admins (unknown ownership).
        let source = match state.objects.get(&object_key(bucket.id, &input.key)) {
            Some(entry) => {
                check_object_access(entry.value(), &found.actor, _scope.as_deref(), is_admin)?;
                Some(entry.value().clone())
            }
            None if !is_admin => {
                return Err(UnifiedError::not_found(format!(
                    "Storage object not found: {}",
                    input.key
                )));
            }
            None => None,
        };
        if let Some(object) = &source {
            if object.size_bytes > MAX_UPLOAD_BYTES as i64 {
                return Err(UnifiedError::new(
                    format!(
                        "Object exceeds the maximum move size of {MAX_UPLOAD_BYTES} bytes"
                    ),
                    413,
                ));
            }
        }
        // Destination must not already exist unless the caller
        // explicitly asked to overwrite it. Existence is checked
        // against the recorded hub rows (the authoritative state);
        // provider-side keys never recorded through the hub are
        // not visible here, so an overwrite can still clobber an
        // unrecorded provider key — that remainder is documented
        // in the module header.
        let destination = state
            .objects
            .get(&object_key(bucket.id, &input.new_key))
            .map(|entry| entry.value().clone());
        move_destination_check(destination.is_some(), input.overwrite, &input.new_key)?;
        // Quota check on the NET delta of the move (see
        // `move_quota_delta`): a plain move is quota-neutral, an
        // overwrite frees the size difference.
        let source_size = source.as_ref().map(|object| object.size_bytes).unwrap_or(0);
        let (delta_bytes, delta_files) = move_quota_delta(
            source_size,
            destination.as_ref().map(|object| object.size_bytes),
        );
        // Quotas are checked against the object's own company
        // when it has one (an admin moving another tenant's
        // object must not charge the admin's company).
        let quota_company = source
            .as_ref()
            .and_then(|object| object.company_id.as_deref())
            .or(_scope.as_deref());
        {
            let _guard = state
                .quota_guard
                .lock()
                .expect("quota guard lock poisoned");
            let check = check_quotas(
                &state,
                source
                    .as_ref()
                    .map(|object| object.owner_module.as_str())
                    .unwrap_or("files"),
                &found.actor,
                quota_company,
                Some(bucket.provider_kind.as_str()),
                delta_bytes,
                delta_files,
            );
            if check.exceeded {
                return Err(UnifiedError::conflict(check.detail)
                    .with_metadata("quota_exceeded", json!(true))
                    .with_metadata("scope", json!(check.scope))
                    .with_metadata("scope_id", json!(check.scope_id)));
            }
        }
        // A move is download + upload + delete through the provider
        // contract (no provider exposes a native rename). The source
        // bytes are held in memory, so moves are capped at the same
        // 100 MiB as uploads.
        let provider = provider_for_bucket(&ctx, &bucket)?;
        let bytes = provider.download(&input.key).await?;
        if bytes.len() > MAX_UPLOAD_BYTES {
            return Err(UnifiedError::new(
                format!(
                    "Object exceeds the maximum move size of {MAX_UPLOAD_BYTES} bytes"
                ),
                413,
            ));
        }
        provider.upload(&input.new_key, bytes.clone()).await?;
        // The source is deleted only after the destination write
        // succeeded, so a failed upload never loses the object.
        // If the source delete fails, the move is rolled back by
        // deleting the just-written destination, so the provider
        // and the recorded state agree again; only when the
        // rollback itself fails is the partial state reported
        // (metadata `partial_move`, naming the destination key so
        // an operator can reconcile it — the recorded state still
        // points at the source key).
        if let Err(error) = provider.delete(&input.key).await {
            match provider.delete(&input.new_key).await {
                Ok(()) => return Err(error),
                Err(rollback_error) => {
                    return Err(UnifiedError::internal(format!(
                        "Move failed to delete the source object and the destination rollback also failed (destination \"{}\" may still exist on the provider; recorded state still points at the source key): source delete: {}; rollback delete: {}",
                        input.new_key, error.message, rollback_error.message
                    ))
                    .with_metadata("partial_move", json!(true))
                    .with_metadata("destination_key", json!(input.new_key)));
                }
            }
        }
        let (mime, module, user, company) =
            match source {
                Some(object) => (
                    object.mime.clone(),
                    object.owner_module.clone(),
                    object.owner_user.clone(),
                    object.company_id.clone(),
                ),
                None => (
                    "application/octet-stream".to_string(),
                    "files".to_string(),
                    found.actor.clone(),
                    _scope.clone(),
                ),
            };
        record_deleted(&state, bucket.id, &input.key);
        record_stored(
            &state,
            bucket.id,
            &input.new_key,
            bytes.len() as i64,
            Some(sha256_hex(&bytes)),
            &mime,
            &module,
            &user,
            company.as_deref(),
            true,
        );
        state.emit(
            events::OBJECT_STORED,
            json!({
                "bucket_id": bucket.id.to_string(),
                "key": input.new_key,
                "size_bytes": bytes.len(),
                "module": module,
                "owner_user": user,
                "company_id": company,
                "moved_from": input.key,
            }),
        );
        state.emit(
            events::OBJECT_DELETED,
            json!({
                "bucket_id": bucket.id.to_string(),
                "key": input.key,
                "deleted_by": found.actor,
                "moved_to": input.new_key,
            }),
        );
        Ok(json!({
            "bucket_id": bucket.id,
            "key": input.new_key,
            "moved_from": input.key,
            "size_bytes": bytes.len(),
        }))
    })()
    .await;
    json::respond(result, debug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_destination_check_refuses_silent_clobber() {
        assert!(move_destination_check(false, false, "b.txt").is_ok());
        assert!(move_destination_check(false, true, "b.txt").is_ok());
        // An existing destination needs an explicit overwrite flag.
        let error = move_destination_check(true, false, "b.txt").unwrap_err();
        assert_eq!(error.code, 409);
        assert!(move_destination_check(true, true, "b.txt").is_ok());
    }

    #[test]
    fn move_quota_delta_is_net_not_gross() {
        // A plain move is quota-neutral: the same bytes just change key.
        assert_eq!(move_quota_delta(1000, None), (0, 0));
        // Overwriting a smaller destination frees only the difference.
        assert_eq!(move_quota_delta(1000, Some(400)), (600, 0));
        // Overwriting a larger destination frees nothing net (negative
        // delta is fine — the quota counters clamp at zero on apply).
        assert_eq!(move_quota_delta(100, Some(400)), (-300, 0));
        // A move never changes the file count.
        assert_eq!(move_quota_delta(1000, Some(1000)).1, 0);
    }
}
