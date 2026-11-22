// Storage module — HTTP access to the storage targets.
//
// Routes under `/api/v1/storage`:
//
//   GET    /                              storage status (kinds, target count)
//   GET    /targets                       list targets (paginated, secrets masked)
//   POST   /targets                       add a target (kind + config, secrets write-only)
//   GET    /targets/{name}                one target (secrets masked)
//   DELETE /targets/{name}                delete a target
//   POST   /targets/{name}/test           test the target connection
//   POST   /targets/{name}/objects?key=   upload object bytes (raw body)
//   GET    /targets/{name}/objects?key=   download object bytes (raw body)
//   DELETE /targets/{name}/objects?key=   delete an object
//   POST   /targets/{name}/signed-url     {"key", "expires_secs"?} -> {"url", ...}
//
// Targets added here are registered in the shared `StorageRegistry`
// on `AppContext` and live in memory; durable targets are declared in
// the `storage` configuration section and registered at startup.
// A target's configuration is never returned with secrets: secret
// fields are masked (see `StorageTargetConfig::masked_config`).
// Object downloads return raw bytes, not the JSON envelope, because
// the envelope cannot carry a binary body.
//
// Capability checks: every handler requires the caller identity
// (`X-User-Id` + `X-Role` headers) and one capability, checked
// through the `role.check` hook (fails closed): `storage.read` for
// reads, `storage.write` for object uploads / deletes / signed URLs,
// `storage.manage_targets` for target create / delete / test.
//
// Integrity: every upload records the SHA-256 of the bytes (returned
// in the response and in the `storage.object.uploaded` event). A
// download with `?expected_sha256=` verifies the bytes against that
// digest and refuses corrupt bytes instead of serving them; local
// and SFTP providers additionally verify against their durable
// sidecar on every plain download.
//
// Quotas: this is the raw provider API — it has no owner or tenant
// scope, so the hub quotas (storage-manager) cannot apply here.
// Targets created through this API may carry an optional
// `quota_bytes` / `max_objects` in their config, enforced here on
// every upload (409 on exceed, overwrite counts only the size
// delta). Quota-sensitive uploads with an owner scope must go
// through the storage-manager hub instead. Targets declared in
// configuration carry no target-level quota here.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::storage::{
    provider_from_config, sha256_hex, validate_target_name, StorageKind, StorageTargetConfig,
};
use actix_web::{http::header, web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{Arc, OnceLock};

/// Maximum object upload size through the API (100 MiB). Larger
/// objects must be uploaded through the provider directly or in
/// chunks by the caller.
const MAX_UPLOAD_BYTES: usize = 100 * 1024 * 1024;

/// Default signed-URL lifetime in seconds.
const DEFAULT_SIGNED_URL_SECS: u64 = 3600;
/// Maximum signed-URL lifetime in seconds (7 days, the S3 presign cap).
const MAX_SIGNED_URL_SECS: u64 = 604_800;

// ---------------------------------------------------------------------------
// Capabilities (checked through the `role.check` hook; fails closed).
// ---------------------------------------------------------------------------

const PERM_READ: &str = "storage.read";
const PERM_WRITE: &str = "storage.write";
const PERM_MANAGE_TARGETS: &str = "storage.manage_targets";

fn authorize(ctx: &AppContext, req: &HttpRequest, permission: &str) -> UnifiedResult<()> {
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    // The actor header is required (anonymous callers are refused)
    // but the capability check runs on the role only.
    let actor_present = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some();
    if !actor_present {
        return Err(UnifiedError::unauthorized("Missing X-User-Id header"));
    }
    let context = ctx.hooks.emit(
        "role.check",
        serde_json::json!({ "role": role, "permission": permission, "allowed": false }),
    );
    let allowed = context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if allowed {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

// ---------------------------------------------------------------------------
// Target metadata store (in-memory, like the downloader job store).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct StoredTarget {
    name: String,
    kind: StorageKind,
    /// Target configuration with secret fields masked.
    config: Value,
    /// Optional hard quota for this target, in bytes (`None` means
    /// no target-level limit). Enforced on every upload.
    quota_bytes: Option<i64>,
    /// Optional hard quota for this target, in objects (`None`
    /// means no target-level limit).
    max_objects: Option<i64>,
    created_at: DateTime<Utc>,
}

static TARGETS: OnceLock<DashMap<String, StoredTarget>> = OnceLock::new();

fn targets_store() -> &'static DashMap<String, StoredTarget> {
    TARGETS.get_or_init(DashMap::new)
}

/// Recorded usage per target, for the optional target-level quota:
/// total bytes and object count of objects uploaded through this
/// module. Keys are target names.
#[derive(Debug, Clone, Copy, Default)]
struct TargetUsage {
    bytes: i64,
    objects: i64,
}

static TARGET_USAGE: OnceLock<DashMap<String, TargetUsage>> = OnceLock::new();

fn target_usage_store() -> &'static DashMap<String, TargetUsage> {
    TARGET_USAGE.get_or_init(DashMap::new)
}

/// Recorded size per object uploaded through this module, keyed by
/// `{target}\n{key}`. Lets overwrites count only the size delta
/// against the quota instead of double-counting.
static OBJECT_SIZES: OnceLock<DashMap<String, i64>> = OnceLock::new();

fn object_sizes_store() -> &'static DashMap<String, i64> {
    OBJECT_SIZES.get_or_init(DashMap::new)
}

fn object_size_key(target: &str, key: &str) -> String {
    format!("{target}\n{key}")
}

// ---------------------------------------------------------------------------
// Views and request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct StorageStatusView {
    kinds: Vec<String>,
    targets: usize,
    target_names: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CreateTargetRequest {
    name: String,
    kind: StorageKind,
    /// Target configuration fields (endpoint, region, bucket,
    /// base_dir, access_key, secret_key, client_id, client_secret,
    /// refresh_token, access_token, token_url). Secrets are
    /// write-only: they are accepted here and never returned.
    #[serde(default)]
    config: serde_json::Map<String, Value>,
}

#[derive(Debug, Deserialize)]
struct ObjectQuery {
    key: String,
    /// Optional expected SHA-256 hex digest: when present the
    /// download is verified against it and corrupt bytes are
    /// refused instead of served.
    #[serde(default)]
    expected_sha256: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SignedUrlRequest {
    key: String,
    #[serde(default)]
    expires_secs: Option<u64>,
}

/// Build a `StorageTargetConfig` from an add-target request: the
/// request `name` and `kind` win, the remaining fields come from the
/// request `config` object.
fn target_config_from_request(request: &CreateTargetRequest) -> UnifiedResult<StorageTargetConfig> {
    validate_target_name(request.name.trim())?;
    let mut object = serde_json::Map::new();
    object.insert(
        "name".to_string(),
        Value::String(request.name.trim().to_string()),
    );
    object.insert(
        "kind".to_string(),
        Value::String(request.kind.as_str().to_string()),
    );
    for (key, value) in &request.config {
        object.insert(key.clone(), value.clone());
    }
    let config: StorageTargetConfig =
        serde_json::from_value(Value::Object(object)).map_err(|error| {
            UnifiedError::bad_request(format!("Invalid storage target config: {error}"))
        })?;
    config.validate()?;
    Ok(config)
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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

fn provider_for(
    ctx: &Arc<AppContext>,
    name: &str,
) -> UnifiedResult<Arc<dyn crate::core::storage::StorageProvider>> {
    ctx.storage_registry
        .get(name)
        .ok_or_else(|| UnifiedError::not_found(format!("Storage target \"{name}\" not found")))
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

async fn get_status(ctx: web::Data<Arc<AppContext>>, req: HttpRequest) -> HttpResponse {
    let debug = ctx.is_debug();
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

async fn list_targets(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
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

async fn create_target(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateTargetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
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
        ctx.storage_registry
            .register(config.name.clone(), provider);
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
            crate::core::storage::events::TARGET_CREATED,
            serde_json::json!({ "target": stored.name, "kind": stored.kind.as_str() }),
        );
        Ok(stored)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_target(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    name: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<StoredTarget> = (|| {
        let name = name.into_inner();
        targets_store()
            .get(&name)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Storage target \"{name}\" not found"))
            })
    })();
    json::respond(result, debug)
}

async fn delete_target(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    name: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE_TARGETS) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let name = name.into_inner();
        if !targets_store().contains_key(&name)
            && ctx.storage_registry.get(&name).is_none()
        {
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
            crate::core::storage::events::TARGET_DELETED,
            serde_json::json!({ "target": name }),
        );
        Ok(serde_json::json!({ "name": name, "deleted": true }))
    })();
    json::respond(result, debug)
}

async fn test_target(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    name: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
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

async fn upload_object(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    name: web::Path<String>,
    query: web::Query<ObjectQuery>,
    body: web::Bytes,
) -> HttpResponse {
    let debug = ctx.is_debug();
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
        let size_bytes = body.len() as i64;
        let checksum = sha256_hex(&body);
        // Target-level quota (runtime targets only; see the module
        // header). An overwrite counts only the size delta, and a
        // new object counts one object.
        let size_key = object_size_key(&name, &query.key);
        let previous_size = object_sizes_store()
            .get(&size_key)
            .map(|entry| *entry.value())
            .unwrap_or(0);
        let is_new = !object_sizes_store().contains_key(&size_key);
        let delta_bytes = size_bytes - previous_size;
        let delta_objects = i64::from(is_new);
        if let Some(target) = targets_store().get(&name) {
            let usage = target_usage_store()
                .get(&name)
                .map(|entry| *entry.value())
                .unwrap_or_default();
            if let Some(limit) = target.quota_bytes {
                if usage.bytes + delta_bytes > limit {
                    return Err(UnifiedError::conflict(format!(
                        "Storage target \"{name}\" quota exceeded: used {} + {delta_bytes} exceeds the limit of {limit} bytes",
                        usage.bytes
                    ))
                    .with_metadata("quota_exceeded", Value::Bool(true))
                    .with_metadata("target", Value::String(name.clone())));
                }
            }
            if let Some(limit) = target.max_objects {
                if usage.objects + delta_objects > limit {
                    return Err(UnifiedError::conflict(format!(
                        "Storage target \"{name}\" object quota exceeded: used {} + {delta_objects} exceeds the limit of {limit} objects",
                        usage.objects
                    ))
                    .with_metadata("quota_exceeded", Value::Bool(true))
                    .with_metadata("target", Value::String(name.clone())));
                }
            }
        }
        let provider = provider_for(&ctx, &name)?;
        let stored_key = provider
            .upload(&query.key, body.to_vec())
            .await?;
        object_sizes_store().insert(size_key, size_bytes);
        target_usage_store()
            .entry(name.clone())
            .and_modify(|usage| {
                usage.bytes += delta_bytes;
                usage.objects += delta_objects;
            })
            .or_insert(TargetUsage {
                bytes: delta_bytes,
                objects: delta_objects,
            });
        ctx.hooks.emit(
            crate::core::storage::events::OBJECT_UPLOADED,
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

async fn download_object(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    name: web::Path<String>,
    query: web::Query<ObjectQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
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

async fn delete_object(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    name: web::Path<String>,
    query: web::Query<ObjectQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
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
        }
        ctx.hooks.emit(
            crate::core::storage::events::OBJECT_DELETED,
            serde_json::json!({ "target": name, "key": query.key }),
        );
        Ok(serde_json::json!({ "target": name, "key": query.key, "deleted": true }))
    })()
    .await;
    json::respond(result, debug)
}

async fn signed_url(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    name: web::Path<String>,
    body: web::Json<SignedUrlRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
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

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct StorageModule;

impl Module for StorageModule {
    fn id(&self) -> &'static str {
        "storage"
    }

    fn name(&self) -> &'static str {
        "Storage"
    }

    fn description(&self) -> &'static str {
        "Storage provider abstraction: local, S3, Cloudflare R2, and Google Drive targets with a single object API for backup, assets serving, and the downloader."
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/storage")
                .route("", web::get().to(get_status))
                .route("/targets", web::get().to(list_targets))
                .route("/targets", web::post().to(create_target))
                .route("/targets/{name}", web::get().to(get_target))
                .route("/targets/{name}", web::delete().to(delete_target))
                .route("/targets/{name}/test", web::post().to(test_target))
                .route("/targets/{name}/objects", web::post().to(upload_object))
                .route("/targets/{name}/objects", web::get().to(download_object))
                .route("/targets/{name}/objects", web::delete().to(delete_object))
                .route("/targets/{name}/signed-url", web::post().to(signed_url)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/storage", owner: "storage", description: "Storage status: supported kinds and registered targets (capability storage.read)." },
            RouteInfo { method: "GET", path: "/api/v1/storage/targets", owner: "storage", description: "List storage targets (paginated, secrets masked, capability storage.read)." },
            RouteInfo { method: "POST", path: "/api/v1/storage/targets", owner: "storage", description: "Add a storage target (secrets write-only, optional quota_bytes / max_objects, capability storage.manage_targets)." },
            RouteInfo { method: "GET", path: "/api/v1/storage/targets/{name}", owner: "storage", description: "Get one storage target (secrets masked, capability storage.read)." },
            RouteInfo { method: "DELETE", path: "/api/v1/storage/targets/{name}", owner: "storage", description: "Delete a storage target (capability storage.manage_targets)." },
            RouteInfo { method: "POST", path: "/api/v1/storage/targets/{name}/test", owner: "storage", description: "Test a storage target connection (capability storage.manage_targets)." },
            RouteInfo { method: "POST", path: "/api/v1/storage/targets/{name}/objects", owner: "storage", description: "Upload object bytes through a target (?key=, SHA-256 recorded, target quota enforced, capability storage.write)." },
            RouteInfo { method: "GET", path: "/api/v1/storage/targets/{name}/objects", owner: "storage", description: "Download object bytes through a target (?key=, ?expected_sha256= verifies, capability storage.read)." },
            RouteInfo { method: "DELETE", path: "/api/v1/storage/targets/{name}/objects", owner: "storage", description: "Delete an object through a target (?key=, capability storage.write)." },
            RouteInfo { method: "POST", path: "/api/v1/storage/targets/{name}/signed-url", owner: "storage", description: "Generate a signed URL for an object (capability storage.write)." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(StorageModule));
}
