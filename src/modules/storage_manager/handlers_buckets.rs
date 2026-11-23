// Bucket handlers.

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

// ---------------------------------------------------------------------------
// Buckets.
// ---------------------------------------------------------------------------

/// Encrypt a credential map (plain JSON object) for storage. Only
/// string values are kept; anything else is rejected so a caller
/// cannot smuggle structured data into the credential blob.
/// Encryption is fail-closed (see `encryption_secret_for_encrypt`)
/// and the fingerprint is taken over the ciphertext.
pub(crate) fn encrypt_credentials(
    ctx: &AppContext,
    credentials: &serde_json::Map<String, Value>,
) -> UnifiedResult<(String, String)> {
    let mut plain = serde_json::Map::new();
    for (key, value) in credentials {
        let text = value.as_str().ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Credential field \"{key}\" must be a string"
            ))
        })?;
        if key.trim().is_empty() || key.len() > 128 {
            return Err(UnifiedError::bad_request(
                "Credential field names must be 1..=128 characters",
            ));
        }
        plain.insert(key.clone(), Value::String(text.to_string()));
    }
    let plaintext = serde_json::to_string(&Value::Object(plain)).map_err(|_| {
        UnifiedError::internal("Cannot encode bucket credentials")
    })?;
    // Fail-closed encryption: refuses when no real secret is
    // configured outside development mode (see crypto.rs).
    let secret = encryption_secret_for_encrypt(&ctx.config)?;
    let encrypted = encrypt_secret_value(&secret, &plaintext);
    // Fingerprint the ciphertext, never the plaintext: a hash of
    // the plaintext is brute-forceable for low-entropy secrets.
    let fingerprint = ciphertext_fingerprint(&encrypted);
    Ok((encrypted, fingerprint))
}

pub(crate) fn validate_bucket_fields(
    ctx: &AppContext,
    bucket: &StorageBucket,
) -> UnifiedResult<()> {
    // Building the provider validates the kind-specific fields
    // (endpoint + allowlist for S3/R2, host allowlist for SFTP, and
    // so on). A bucket that cannot build a provider cannot be
    // created: fail here, before anything is stored. Only the
    // operator-configured allowlist extends the endpoint allowlist
    // (see `provider_for_bucket_with`); the bucket's own list is
    // validated against it at creation/update time.
    let config = target_config_for_bucket(ctx, bucket)?;
    let operator_hosts = operator_allowed_hosts(&ctx.config);
    provider_from_config(&config, &operator_hosts).map(|_| ())
}

pub(crate) async fn list_buckets(
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
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let params = params.into_inner();
    let mut items: Vec<BucketView> = state
        .buckets
        .iter()
        .map(|entry| BucketView::from(entry.value()))
        .collect();
    // Tenant scoping: a non-admin lists only shared buckets and
    // buckets of their own company; an admin lists all.
    let is_admin = is_storage_admin(&ctx, &found.role);
    items.retain(|bucket| bucket_company_visible(&bucket.company_id, &_scope, is_admin));
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|bucket| {
            bucket.name.to_lowercase().contains(&needle)
                || bucket.container.to_lowercase().contains(&needle)
        });
    }
    if let Some(kind) = params.filters.get("provider_kind") {
        items.retain(|bucket| bucket.provider_kind.as_str() == kind.as_str());
    }
    if let Some(company) = params.filters.get("company_id") {
        items.retain(|bucket| bucket.company_id.as_deref() == Some(company.as_str()));
    }
    if let Some(enabled) = params.filters.get("enabled") {
        let want = enabled == "true";
        items.retain(|bucket| bucket.enabled == want);
    }
    let sort = params.allowed_sort(&["name", "created_at"], "name");
    let descending = params.normalized().order == "desc";
    items.sort_by(|a, b| {
        let ordering = match sort.as_str() {
            "created_at" => a.created_at.cmp(&b.created_at),
            _ => a.name.cmp(&b.name),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    ok(paginate(items, &params))
}

pub(crate) async fn create_bucket(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateBucketRequest>,
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
    let result: UnifiedResult<BucketView> = (|| {
        let input = body.into_inner();
        let name = require_text(&input.name, "name", 128)?;
        if state
            .buckets
            .iter()
            .any(|entry| entry.value().name == name)
        {
            return Err(UnifiedError::conflict(format!(
                "Storage bucket \"{name}\" already exists"
            )));
        }
        let endpoint = optional_text(&input.endpoint, "endpoint", 1024)?.unwrap_or_default();
        let region = optional_text(&input.region, "region", 128)?.unwrap_or_default();
        let container = optional_text(&input.container, "container", 256)?.unwrap_or_default();
        let path = optional_text(&input.path, "path", 1024)?.unwrap_or_default();
        let sftp_host = optional_text(&input.sftp_host, "sftp_host", 253)?.unwrap_or_default();
        let sftp_username =
            optional_text(&input.sftp_username, "sftp_username", 64)?.unwrap_or_default();
        let sftp_auth_kind = match input.sftp_auth_kind {
            Some(raw) => require_text(&raw, "sftp_auth_kind", 32)?,
            None => String::new(),
        };
        let sftp_keyfile =
            optional_text(&input.sftp_keyfile, "sftp_keyfile", 4096)?.unwrap_or_default();
        let mut allowed_hosts: Vec<String> = Vec::new();
        if let Some(hosts) = input.allowed_hosts {
            for host in hosts {
                let host = require_text(&host, "allowed_hosts entry", 253)?;
                allowed_hosts.push(host.to_ascii_lowercase());
            }
            allowed_hosts.sort();
            allowed_hosts.dedup();
            // Bucket-supplied hosts may only narrow the operator
            // allowlist, never extend it: an arbitrary host here
            // must be rejected, not stored.
            validate_requested_allowed_hosts(
                &operator_allowed_hosts(&ctx.config),
                &allowed_hosts,
            )?;
        }
        // Tenant scoping: the bucket's company is verified against
        // the caller's scope — a scoped caller creates a bucket for
        // their own company (an omitted company adopts it); an
        // unresolved caller may only create a shared bucket.
        let body_company = optional_text(&input.company_id, "company_id", 128)?;
        let company_id = require_company_match_for(&_scope, &body_company)?;
        let (credentials_encrypted, key_fingerprint) = match &input.credentials {
            Some(map) => {
                let (encrypted, fingerprint) = encrypt_credentials(&ctx, map)?;
                (Some(encrypted), Some(fingerprint))
            }
            None => (None, None),
        };
        let now = Utc::now();
        let bucket = StorageBucket {
            id: Uuid::now_v7(),
            name,
            provider_kind: input.provider_kind,
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
            encryption_at_rest: input.encryption_at_rest.unwrap_or(false),
            enabled: true,
            company_id,
            created_by: found.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        // Fail before storing when the bucket cannot build a live
        // provider (bad endpoint host, unallowlisted SFTP host,
        // missing credentials, ...).
        validate_bucket_fields(&ctx, &bucket)?;
        let view = BucketView::from(&bucket);
        state.buckets.insert(bucket.id, bucket.clone());
        state.emit(
            events::BUCKET_CREATED,
            json!({
                "bucket_id": bucket.id.to_string(),
                "name": bucket.name,
                "provider_kind": bucket.provider_kind.as_str(),
                "encryption_at_rest": bucket.encryption_at_rest,
                "created_by": found.actor,
            }),
        );
        Ok(view)
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err_response(error, debug),
    }
}

pub(crate) async fn get_bucket(
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
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_storage_admin(&ctx, &found.role);
    let result: UnifiedResult<BucketView> = (|| {
        let bucket = load_bucket(&state, &id)?;
        check_bucket_access(&bucket, _scope.as_deref(), is_admin)?;
        Ok(BucketView::from(&bucket))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_bucket(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<UpdateBucketRequest>,
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
    let result: UnifiedResult<BucketView> = (|| {
        let input = body.into_inner();
        let mut bucket = load_bucket(&state, &id)?;
        check_bucket_access(&bucket, _scope.as_deref(), is_admin)?;
        if let Some(raw) = &input.name {
            let name = require_text(raw, "name", 128)?;
            if state
                .buckets
                .iter()
                .any(|entry| entry.value().name == name && entry.value().id != bucket.id)
            {
                return Err(UnifiedError::conflict(format!(
                    "Storage bucket \"{name}\" already exists"
                )));
            }
            bucket.name = name;
        }
        if let Some(raw) = &input.endpoint {
            bucket.endpoint = require_text(raw, "endpoint", 1024)?;
        }
        if let Some(raw) = &input.region {
            bucket.region = require_text(raw, "region", 128)?;
        }
        if let Some(raw) = &input.container {
            bucket.container = require_text(raw, "container", 256)?;
        }
        if let Some(raw) = &input.path {
            bucket.path = require_text(raw, "path", 1024)?;
        }
        if let Some(raw) = &input.sftp_host {
            bucket.sftp_host = require_text(raw, "sftp_host", 253)?;
        }
        if let Some(port) = input.sftp_port {
            bucket.sftp_port = port;
        }
        if let Some(raw) = &input.sftp_username {
            bucket.sftp_username = require_text(raw, "sftp_username", 64)?;
        }
        if let Some(raw) = &input.sftp_auth_kind {
            bucket.sftp_auth_kind = require_text(raw, "sftp_auth_kind", 32)?;
        }
        if let Some(raw) = &input.sftp_keyfile {
            bucket.sftp_keyfile = require_text(raw, "sftp_keyfile", 4096)?;
        }
        if let Some(hosts) = input.allowed_hosts {
            let mut cleaned: Vec<String> = Vec::new();
            for host in hosts {
                cleaned.push(require_text(&host, "allowed_hosts entry", 253)?.to_ascii_lowercase());
            }
            cleaned.sort();
            cleaned.dedup();
            validate_requested_allowed_hosts(
                &operator_allowed_hosts(&ctx.config),
                &cleaned,
            )?;
            bucket.allowed_hosts = cleaned;
        }
        if let Some(map) = &input.credentials {
            if map.is_empty() {
                bucket.credentials_encrypted = None;
                bucket.key_fingerprint = None;
            } else {
                let (encrypted, fingerprint) = encrypt_credentials(&ctx, map)?;
                bucket.credentials_encrypted = Some(encrypted);
                bucket.key_fingerprint = Some(fingerprint);
            }
        }
        if let Some(flag) = input.encryption_at_rest {
            bucket.encryption_at_rest = flag;
        }
        if let Some(enabled) = input.enabled {
            bucket.enabled = enabled;
        }
        if let Some(company) = input.company_id {
            bucket.company_id = match company {
                Some(raw) => Some(require_text(&raw, "company_id", 128)?),
                None => None,
            };
        }
        validate_bucket_fields(&ctx, &bucket)?;
        bucket.updated_at = Utc::now();
        let view = BucketView::from(&bucket);
        state.buckets.insert(bucket.id, bucket.clone());
        state.emit(
            events::BUCKET_UPDATED,
            json!({
                "bucket_id": bucket.id.to_string(),
                "name": bucket.name,
                "provider_kind": bucket.provider_kind.as_str(),
                "enabled": bucket.enabled,
                "updated_by": found.actor,
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_bucket(
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
    let found = match authorize(&ctx, &req, PERM_MANAGE_BUCKETS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_storage_admin(&ctx, &found.role);
    let result: UnifiedResult<Value> = (|| {
        let bucket = load_bucket(&state, &id)?;
        check_bucket_access(&bucket, _scope.as_deref(), is_admin)?;
        if state
            .bindings
            .iter()
            .any(|entry| entry.value().bucket_id == bucket.id && entry.value().enabled)
        {
            return Err(UnifiedError::conflict(format!(
                "Storage bucket \"{}\" still has enabled bindings; delete or re-point them first",
                bucket.name
            )));
        }
        if state
            .lifecycle_rules
            .iter()
            .any(|entry| entry.value().bucket_id == bucket.id)
        {
            return Err(UnifiedError::conflict(format!(
                "Storage bucket \"{}\" still has lifecycle rules; delete them first",
                bucket.name
            )));
        }
        // Recorded objects are metadata only; the provider objects
        // themselves are left in place (deleting a bucket must never
        // silently delete stored bytes). The metadata rows go with
        // the bucket so the browser cannot show a deleted bucket.
        state
            .objects
            .retain(|_, object| object.bucket_id != bucket.id);
        state.buckets.remove(&bucket.id);
        // Defaults pointing at the deleted bucket fall back to none
        // rather than dangling.
        {
            let mut global = state
                .global_default
                .write()
                .expect("global default lock poisoned");
            if *global == Some(bucket.id) {
                *global = None;
            }
        }
        state
            .company_defaults
            .retain(|_, value| *value != bucket.id);
        state.emit(
            events::BUCKET_DELETED,
            json!({
                "bucket_id": bucket.id.to_string(),
                "name": bucket.name,
                "deleted_by": found.actor,
            }),
        );
        Ok(json!({ "bucket_id": bucket.id, "name": bucket.name, "deleted": true }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn test_bucket(
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
    let found = match authorize(&ctx, &req, PERM_MANAGE_BUCKETS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_storage_admin(&ctx, &found.role);
    let result: UnifiedResult<Value> = (|| async {
        let bucket = load_bucket(&state, &id)?;
        check_bucket_access(&bucket, _scope.as_deref(), is_admin)?;
        let provider = provider_for_bucket(&ctx, &bucket)?;
        provider.test_connection().await?;
        Ok(json!({
            "bucket_id": bucket.id,
            "name": bucket.name,
            "provider_kind": bucket.provider_kind.as_str(),
            "ok": true,
        }))
    })()
    .await;
    json::respond(result, debug)
}
