// Signed URL handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use unified_core::storage::validate_key;
use actix_web::{http::header, web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Signed URLs (revocable hub download tokens).
// ---------------------------------------------------------------------------

pub(crate) async fn issue_signed_url(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<IssueSignedUrlRequest>,
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
    let found = match authorize(&ctx, &req, PERM_MANAGE_SIGNED_URLS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_storage_admin(&ctx, &found.role);
    let result: UnifiedResult<Value> = (|| async {
        let bucket = load_bucket(&state, &id)?;
        check_bucket_access(&bucket, _scope.as_deref(), is_admin)?;
        validate_key(&body.key)?;
        let expires_secs = body
            .expires_secs
            .unwrap_or(DEFAULT_SIGNED_URL_SECS)
            .clamp(1, MAX_SIGNED_URL_SECS);
        let expires_at = Utc::now() + chrono::Duration::seconds(expires_secs as i64);
        // A provider (presigned) URL is a bonus when the provider
        // supports it. Local and SFTP honestly refuse; the hub token
        // below is the revocable path for them.
        let provider = provider_for_bucket(&ctx, &bucket)?;
        let (provider_url, provider_url_issued) = match provider.signed_url(&body.key, expires_secs).await {
            Ok(url) => (Some(url), true),
            Err(_) => (None, false),
        };
        // The hub token is a bearer secret: it is returned once here
        // and never listed again. Only the SHA-256 hash of the token
        // is stored on the record (never the token value itself),
        // so a copy of the store never leaks a usable token.
        let token = Uuid::new_v4().to_string();
        let token_hash = signed_url_token_hash(&token);
        let now = Utc::now();
        let record = SignedUrlRecord {
            id: Uuid::now_v7(),
            token: String::new(),
            token_hash,
            bucket_id: bucket.id,
            key: body.key.clone(),
            issued_by: found.actor.clone(),
            expires_at,
            revoked_at: None,
            revoke_reason: None,
            provider_url_issued,
            used_at: None,
            created_at: now,
            updated_at: now,
        };
        state.signed_urls.insert(record.id, record.clone());
        state.emit(
            events::SIGNED_URL_ISSUED,
            json!({
                "signed_url_id": record.id.to_string(),
                "bucket_id": bucket.id.to_string(),
                "key": body.key,
                "issued_by": found.actor,
                "expires_at": expires_at.to_rfc3339(),
                "provider_url_issued": provider_url_issued,
            }),
        );
        Ok(json!({
            "signed_url_id": record.id,
            "token": token,
            "download_url": format!("/api/v1/storage-manager/download/{token}"),
            "provider_url": provider_url,
            "bucket_id": bucket.id,
            "key": body.key,
            "expires_secs": expires_secs,
            "expires_at": expires_at,
        }))
    })()
    .await;
    match result {
        Ok(view) => created(view),
        Err(error) => err_response(error, debug),
    }
}

pub(crate) async fn list_signed_urls(
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
    let found = match authorize(&ctx, &req, PERM_MANAGE_SIGNED_URLS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let params = params.into_inner();
    let mut items: Vec<SignedUrlView> = state
        .signed_urls
        .iter()
        .map(|entry| SignedUrlView::from(entry.value()))
        .collect();
    // Tenant scoping: a non-admin lists only signed URLs for
    // buckets they may access; an admin lists all. Records whose
    // bucket is gone are hidden from non-admins.
    let is_admin = is_storage_admin(&ctx, &found.role);
    items.retain(|record| match state.buckets.get(&record.bucket_id) {
        Some(bucket) => bucket_company_visible(&bucket.value().company_id, &_scope, is_admin),
        None => is_admin,
    });
    if let Some(bucket) = params.filters.get("bucket_id") {
        items.retain(|record| record.bucket_id.to_string() == *bucket);
    }
    if let Some(revoked) = params.filters.get("revoked") {
        let want = revoked == "true";
        items.retain(|record| record.revoked_at.is_some() == want);
    }
    if let Some(issuer) = params.filters.get("issued_by") {
        items.retain(|record| record.issued_by == *issuer);
    }
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    ok(paginate(items, &params))
}

pub(crate) async fn revoke_signed_url(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<RevokeSignedUrlRequest>,
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
    let found = match authorize(&ctx, &req, PERM_MANAGE_SIGNED_URLS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_storage_admin(&ctx, &found.role);
    let result: UnifiedResult<SignedUrlView> = (|| {
        let record_id = parse_uuid(&id, "signed URL")?;
        // Take the view (and the fields the event needs) out of the
        // guard's scope first: the guard must be dropped before
        // emitting, so a hook handler can never deadlock on the map.
        let (view, bucket_id, key, reason) = {
            let mut record = state
                .signed_urls
                .get_mut(&record_id)
                .ok_or_else(|| {
                    UnifiedError::not_found(format!("Signed URL not found: {record_id}"))
                })?;
            let bucket = load_bucket(&state, &record.bucket_id.to_string())?;
            check_bucket_access(&bucket, _scope.as_deref(), is_admin)?;
            if record.revoked_at.is_some() {
                return Err(UnifiedError::conflict(
                    "This signed URL is already revoked",
                ));
            }
            let reason = match &body.reason {
                Some(raw) => Some(require_text(raw, "reason", 500)?),
                None => None,
            };
            record.revoked_at = Some(Utc::now());
            record.revoke_reason = reason.clone();
            record.updated_at = Utc::now();
            let view = SignedUrlView::from(&*record);
            (
                view,
                record.bucket_id,
                record.key.clone(),
                reason,
            )
        };
        state.emit(
            events::SIGNED_URL_REVOKED,
            json!({
                "signed_url_id": record_id.to_string(),
                "bucket_id": bucket_id.to_string(),
                "key": key,
                "revoked_by": found.actor,
                "reason": reason,
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

/// SHA-256 hex of a hub download token, the lookup key for
/// redemption. The token value itself is never stored.
pub(crate) fn signed_url_token_hash(token: &str) -> String {
    sha256_hex_of(token.as_bytes())
}

/// True when the presented token matches the record's stored hash,
/// compared in constant time over the hash bytes.
pub(crate) fn signed_url_token_matches(record: &SignedUrlRecord, presented_token: &str) -> bool {
    let presented_hash = signed_url_token_hash(presented_token);
    constant_time_eq(record.token_hash.as_bytes(), presented_hash.as_bytes())
}

/// True when a token record may still be redeemed: not revoked,
/// not used, and not expired. Single-use tokens that were already
/// consumed report as revoked-with-reason, not as "not found".
pub(crate) fn is_redeemable(record: &SignedUrlRecord, now: DateTime<Utc>) -> bool {
    record.revoked_at.is_none() && record.used_at.is_none() && record.expires_at > now
}

/// Redeem a hub download token. No capability check here by design:
/// possession of the token IS the authorization, exactly like a
/// presigned URL. Expiry and revocation are enforced on every
/// redemption; the token value is never logged. Tokens are
/// single-use: a successful download consumes (revokes) the token,
/// so a replayed token is refused.
pub(crate) async fn redeem_download(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    token: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let token = token.into_inner();
    // Hash-keyed lookup with a constant-time compare: the presented
    // token is hashed once and matched against stored hashes —
    // never a linear `==` scan over token values.
    let presented_hash = signed_url_token_hash(&token);
    let record = state
        .signed_urls
        .iter()
        .find(|entry| {
            constant_time_eq(
                entry.value().token_hash.as_bytes(),
                presented_hash.as_bytes(),
            )
        })
        .map(|entry| entry.value().clone());
    let Some(record) = record else {
        return err_response(
            UnifiedError::not_found("Download token not found"),
            debug,
        );
    };
    if record.revoked_at.is_some() {
        return err_response(
            UnifiedError::forbidden("This download token has been revoked"),
            debug,
        );
    }
    if record.used_at.is_some() {
        return err_response(
            UnifiedError::forbidden("This download token has already been used"),
            debug,
        );
    }
    if record.expires_at <= Utc::now() {
        return err_response(
            UnifiedError::new("This download token has expired", 410),
            debug,
        );
    }
    let Some(bucket) = state.buckets.get(&record.bucket_id) else {
        return err_response(
            UnifiedError::not_found("The bucket for this download token no longer exists"),
            debug,
        );
    };
    let bucket = bucket.value().clone();
    let provider = match provider_for_bucket(&ctx, &bucket) {
        Ok(provider) => provider,
        Err(error) => return err_response(error, debug),
    };
    match provider.download(&record.key).await {
        Ok(bytes) => {
            // Single-use: consume the token on a successful
            // download, before returning the bytes, so a replay
            // (or a second concurrent redemption) is refused.
            if let Some(mut stored) = state.signed_urls.get_mut(&record.id) {
                stored.used_at = Some(Utc::now());
                stored.updated_at = Utc::now();
            }
            let file_name = record
                .key
                .rsplit('/')
                .next()
                .unwrap_or(&record.key)
                .replace('"', "");
            HttpResponse::Ok()
                .content_type("application/octet-stream")
                .insert_header((
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{file_name}\""),
                ))
                .body(bytes)
        }
        Err(error) => err_response(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_record(token: &str, expires_at: DateTime<Utc>) -> SignedUrlRecord {
        let now = Utc::now();
        SignedUrlRecord {
            id: Uuid::now_v7(),
            token: String::new(),
            token_hash: signed_url_token_hash(token),
            bucket_id: Uuid::now_v7(),
            key: "a/b.txt".to_string(),
            issued_by: "u-1".to_string(),
            expires_at,
            revoked_at: None,
            revoke_reason: None,
            provider_url_issued: false,
            used_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn token_lookup_is_hash_keyed_and_constant_time() {
        let record = test_record("secret-token", Utc::now() + chrono::Duration::hours(1));
        // The token value itself is never stored on the record.
        assert!(record.token.is_empty());
        assert_eq!(record.token_hash, signed_url_token_hash("secret-token"));
        assert!(signed_url_token_matches(&record, "secret-token"));
        assert!(!signed_url_token_matches(&record, "secret-tokeo"));
        assert!(!signed_url_token_matches(&record, ""));
        // Two different tokens hash differently.
        assert_ne!(
            signed_url_token_hash("token-a"),
            signed_url_token_hash("token-b")
        );
    }

    #[test]
    fn single_use_tokens_are_not_redeemable_after_use() {
        let now = Utc::now();
        let fresh = test_record("t", now + chrono::Duration::hours(1));
        assert!(is_redeemable(&fresh, now));
        let mut used = fresh.clone();
        used.used_at = Some(now);
        assert!(!is_redeemable(&used, now));
        let mut revoked = fresh.clone();
        revoked.revoked_at = Some(now);
        assert!(!is_redeemable(&revoked, now));
        let expired = test_record("t", now - chrono::Duration::seconds(1));
        assert!(!is_redeemable(&expired, now));
    }
}
