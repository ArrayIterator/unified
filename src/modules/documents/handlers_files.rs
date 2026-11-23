// documents — HTTP handlers: /files/* direct file serving.
//
// Two routes, one pattern, two opposite trust models:
// - GET /files/public/{key}: ZERO checks — no auth, no DB read.
//   The key's first segment IS the visibility decision (the object
//   lives in the tenant's public area). Found -> serve; not found
//   -> fall through to the next middleware (the app's normal 404
//   handling — the same JSON envelope + redirect check the
//   server's default 404 produces; this handler generates no 404
//   body of its own).
// - GET /files/private/{key}: the access lock. Rule 1 — load the
//   DB row + visibility; public -> skip the header check, serve.
//   Rule 2 — private -> check the header (auth). Rule 3 — valid
//   -> proceed. Rule 4 — found -> serve, not found -> 404. ALL
//   failures -> 404 uniform (invalid auth is also 404, never
//   401/403), with an S3-style XML error body.
//
// The API is JSON-only: no file route lives under /api/vX and
// /files/* has no version segment, so file URLs never change.
// `?download=1` on either route sends the file as an attachment
// (default is inline); both routes stream in 64 KiB chunks and
// never buffer a whole file.

use super::*;
use actix_web::{HttpRequest, HttpResponse, web};
use bytes::Bytes;
use futures_util::stream;
use serde::Deserialize;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::json;
use unified_core::storage::{StorageKind, validate_key};
use unified_core::tenancy::ControlPlaneGuard;
use uuid::Uuid;

/// Chunk size for streamed file bodies (64 KiB).
const FILE_STREAM_CHUNK: usize = 65536;

/// Query parameters for the /files/* routes: an optional signed
/// token (private route) and the download flag.
#[derive(Debug, Deserialize)]
pub(crate) struct FileQuery {
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub download: Option<String>,
}

impl FileQuery {
    /// `?download=1` (or `?download=true`) asks for an
    /// attachment; anything else is inline.
    fn wants_download(&self) -> bool {
        matches!(self.download.as_deref(), Some("1") | Some("true"))
    }
}

/// S3-style XML error body for private-route failures (uniform
/// 404 — the body never says which check failed).
fn xml_not_found() -> HttpResponse {
    HttpResponse::NotFound()
        .content_type("application/xml")
        .body(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message></Error>",
        )
}

/// The fall-through 404 for the public route: exactly what the
/// server's default 404 produces (redirect for a recorded old
/// path, else the standard JSON error envelope). Returning it is
/// the observable behaviour of falling through to the next
/// middleware.
fn public_not_found(ctx: &AppContext, req: &HttpRequest) -> HttpResponse {
    if let Some(target) = ctx.routes.resolve_redirect(req.path()) {
        return HttpResponse::MovedPermanently()
            .insert_header((actix_web::http::header::LOCATION, target))
            .finish();
    }
    json::error_response(
        &UnifiedError::not_found("Route not found"),
        ctx.is_debug(),
    )
}

/// A parsed `Range` header: one inclusive byte range, or the
/// reason the header is unusable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RangeRequest {
    /// No `Range` header: serve the whole object.
    Full,
    /// Serve bytes `start..=end` (inclusive).
    Range(u64, u64),
    /// Invalid, multiple, or unsatisfiable range: 416.
    Unsatisfiable,
}

/// Parse one `Range` header against the object's total size.
/// Supports `start-end`, `start-` (open), and `-suffix` (last N
/// bytes). Multiple ranges are rejected (416, no multipart).
/// An end past the object is clamped to the last byte.
pub(crate) fn parse_range(header: Option<&str>, total: u64) -> RangeRequest {
    let Some(header) = header else {
        return RangeRequest::Full;
    };
    let Some(spec) = header.strip_prefix("bytes=") else {
        return RangeRequest::Unsatisfiable;
    };
    if spec.contains(',') {
        return RangeRequest::Unsatisfiable;
    }
    let (start_s, end_s) = match spec.split_once('-') {
        Some(pair) => pair,
        None => return RangeRequest::Unsatisfiable,
    };
    if start_s.is_empty() {
        // Suffix range: `-N` = the last N bytes.
        let Ok(suffix) = end_s.parse::<u64>() else {
            return RangeRequest::Unsatisfiable;
        };
        if suffix == 0 || total == 0 {
            return RangeRequest::Unsatisfiable;
        }
        let start = total.saturating_sub(suffix);
        return RangeRequest::Range(start, total - 1);
    }
    let Ok(start) = start_s.parse::<u64>() else {
        return RangeRequest::Unsatisfiable;
    };
    if end_s.is_empty() {
        // Open range: `start-` = start to the end.
        if start >= total {
            return RangeRequest::Unsatisfiable;
        }
        return RangeRequest::Range(start, total - 1);
    }
    let Ok(end) = end_s.parse::<u64>() else {
        return RangeRequest::Unsatisfiable;
    };
    if start > end || start >= total {
        return RangeRequest::Unsatisfiable;
    }
    RangeRequest::Range(start, end.min(total - 1))
}

/// Pure decision of the private route's rule 1 + 2: given the
/// document (or its absence) and whether the caller passed the
/// auth checks, decide serve vs uniform-404. Rule 1: a public
/// document serves without any header check. Rule 2: a private
/// document serves only when the caller is authorized. Everything
/// else is a uniform 404 (the reason is never revealed).
pub(crate) fn private_access_decision(
    document: Option<&DocumentRecord>,
    authorized: bool,
) -> bool {
    match document {
        Some(document) if document.visibility == "public" => true,
        Some(_) => authorized,
        None => false,
    }
}

/// Pure token-scope decision for the private route: a signed token
/// grants access only when it names THIS document, and — when the
/// request arrived on a tenant host — when its scope IS that tenant.
/// A token issued for another tenant (or another document) never
/// replays here, even though the token itself verifies.
fn token_grants_access(
    token_claim: &Option<(String, StorageScope)>,
    document: &Option<DocumentRecord>,
    host_tenant_id: Option<Uuid>,
) -> bool {
    match (token_claim, document, host_tenant_id) {
        (Some((doc_id, token_scope)), Some(document), host_tenant_id) => {
            doc_id == &document.id
                && match host_tenant_id {
                    Some(tenant_id) => *token_scope == StorageScope::Tenant(tenant_id),
                    None => true,
                }
        }
        _ => false,
    }
}

/// Rules 2–3 for a private document: the header auth chain
/// (tenant + company scope + read capability + record scope +
/// can_read + download gate). Returns the caller's actor id on
/// success, `None` on any failure — the caller turns it into a
/// uniform 404, never a 401/403.
fn private_caller_actor(
    state: &DocumentsState,
    ctx: &AppContext,
    req: &HttpRequest,
    document: &DocumentRecord,
) -> Option<String> {
    let tenant = require_tenant(req).ok()?;
    let scope = caller_company(&tenant, req).ok()?;
    let found = authorize(ctx, req, PERM_READ).ok()?;
    if require_record_in_company_scope(&document.company_id, &scope).is_err() {
        return None;
    }
    if !state.can_read(ctx, &found, document) {
        return None;
    }
    if state.download_gate(document).is_err() {
        return None;
    }
    Some(found.actor)
}

/// Build the file response for one object: headers + a streamed
/// body. `total` is the authoritative object size (from the
/// provider, not the DB row — an old version's key has an old
/// size). `download_name` is the original filename when known
/// (`Some` -> `inline`/`attachment` carries it; `None` -> no
/// Content-Disposition). `not_found` is returned when the object
/// cannot be opened (a race with a delete between the size check
/// and the open). Local storage streams straight from disk (seek
/// + 64 KiB chunks); other providers serve the range through
/// `download_range` (bounded by the range).
#[allow(clippy::too_many_arguments)]
async fn serve_object(
    state: &Arc<DocumentsState>,
    provider_key: &str,
    mime: &str,
    total: u64,
    download_name: Option<&str>,
    download: bool,
    cache_control: &str,
    range: RangeRequest,
    not_found: HttpResponse,
) -> HttpResponse {
    let (start, end, status) = match range {
        RangeRequest::Full => {
            if total == 0 {
                (0, 0, actix_web::http::StatusCode::OK)
            } else {
                (0, total - 1, actix_web::http::StatusCode::OK)
            }
        }
        RangeRequest::Range(start, end) => {
            (start, end, actix_web::http::StatusCode::PARTIAL_CONTENT)
        }
        RangeRequest::Unsatisfiable => {
            return HttpResponse::RangeNotSatisfiable()
                .append_header(("Content-Range", format!("bytes */{total}")))
                .append_header(("Accept-Ranges", "bytes"))
                .body(Bytes::new());
        }
    };
    let length = if total == 0 { 0 } else { end - start + 1 };
    let mut response = HttpResponse::build(status);
    response
        .content_type(mime.to_string())
        .append_header(("Content-Length", length.to_string()))
        .append_header(("Accept-Ranges", "bytes"))
        .append_header(("Cache-Control", cache_control.to_string()));
    if status == actix_web::http::StatusCode::PARTIAL_CONTENT {
        response.append_header(("Content-Range", format!("bytes {start}-{end}/{total}")));
    }
    if download {
        match download_name {
            Some(name) => {
                response.append_header((
                    "Content-Disposition",
                    format!("attachment; filename=\"{name}\""),
                ));
            }
            None => {
                response.append_header(("Content-Disposition", "attachment"));
            }
        }
    } else if let Some(name) = download_name {
        // Inline, with the original name so Save As offers it.
        response.append_header((
            "Content-Disposition",
            format!("inline; filename=\"{name}\""),
        ));
    }
    let provider = match state.provider() {
        Ok(provider) => provider,
        Err(_) => return not_found,
    };
    if matches!(provider.kind(), StorageKind::Local) {
        let path = match state.local_object_path(provider_key) {
            Ok(path) => path,
            Err(_) => return not_found,
        };
        let mut file = match tokio::fs::File::open(&path).await {
            Ok(file) => file,
            Err(_) => return not_found,
        };
        if start > 0 && file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
            return not_found;
        }
        let file_stream = stream::unfold(
            (file, length),
            |(mut file, mut remaining)| async move {
                if remaining == 0 {
                    return None;
                }
                let mut buffer = vec![0u8; FILE_STREAM_CHUNK.min(remaining as usize)];
                match file.read(&mut buffer).await {
                    Ok(0) => None,
                    Ok(read) => {
                        remaining -= read as u64;
                        buffer.truncate(read);
                        Some((
                            Ok::<Bytes, actix_web::Error>(Bytes::from(buffer)),
                            (file, remaining),
                        ))
                    }
                    Err(error) => Some((
                        Err(actix_web::error::ErrorInternalServerError(error)),
                        (file, remaining),
                    )),
                }
            },
        );
        response.streaming(file_stream)
    } else {
        let bytes = match provider.download_range(provider_key, start, end).await {
            Ok(bytes) => bytes,
            Err(_) => return not_found,
        };
        let chunk_stream = stream::iter(
            bytes
                .chunks(FILE_STREAM_CHUNK)
                .map(|chunk| Ok::<Bytes, actix_web::Error>(Bytes::copy_from_slice(chunk))),
        );
        response.streaming(chunk_stream)
    }
}

/// GET /files/public/{key}: ZERO checks — no auth, no DB read.
/// The tenant scope comes from the host (tenant hosts serve
/// their tenant's public area; the control-plane host serves the
/// system public area). The object size comes from the provider
/// (a stat / HEAD, never a byte read); the Content-Type comes
/// from the extension only (no DB mime, no head sniff — the
/// extension is validated at upload). A miss falls through to the
/// app's normal 404 handling.
pub(crate) async fn serve_public_file(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<FileQuery>,
) -> HttpResponse {
    let key = path.into_inner();
    if validate_key(&key).is_err() {
        return public_not_found(&ctx, &req);
    }
    // Scope from the host only (never a header, never the DB):
    // a tenant host serves its tenant's public area, the
    // control-plane host serves the system public area. A tenant
    // host whose tenant cannot be resolved fails closed.
    let scope = if ControlPlaneGuard::is_control_plane_request(&req) {
        StorageScope::System
    } else {
        match require_tenant(&req) {
            Ok(tenant) => StorageScope::Tenant(tenant.tenant_id),
            Err(_) => return public_not_found(&ctx, &req),
        }
    };
    let provider_key = scope.provider_key_for_area(StorageArea::Public, &key);
    let provider = match state.provider() {
        Ok(provider) => provider,
        Err(_) => return public_not_found(&ctx, &req),
    };
    let total = match provider.object_size(&provider_key).await {
        Ok(total) => total,
        Err(_) => return public_not_found(&ctx, &req),
    };
    // No DB read is allowed here: the original filename is
    // unknowable, so the download name is the key's last segment.
    let download_name = key.rsplit('/').next().map(|name| name.to_string());
    let mime = mime_for(&key, &[]);
    let range = parse_range(
        req.headers()
            .get(actix_web::http::header::RANGE)
            .and_then(|value| value.to_str().ok()),
        total,
    );
    serve_object(
        &state,
        &provider_key,
        &mime,
        total,
        download_name.as_deref(),
        query.wants_download(),
        "public, max-age=31536000, immutable",
        range,
        public_not_found(&ctx, &req),
    )
    .await
}

/// GET /files/private/{key}: the access lock (the 4-rule flow).
/// Rule 1 — load the DB row + visibility; public -> skip the
/// header check, serve. Rule 2 — private -> check the header
/// (auth) or a signed token (`?token=`, the token IS the auth).
/// Rule 3 — valid -> proceed. Rule 4 — found -> serve, not found
/// -> 404. ALL failures -> 404 uniform (invalid auth is also
/// 404, never 401/403), with an S3-style XML error body. The
/// scope comes from the host tenant, else from a signed token
/// (token redemptions carry their own scope, like the old
/// download_signed route); a token that names another document
/// or another scope is not valid for this file.
pub(crate) async fn serve_private_file(
    state: web::Data<Arc<DocumentsState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<FileQuery>,
) -> HttpResponse {
    let key = path.into_inner();
    if validate_key(&key).is_err() {
        return xml_not_found();
    }
    // Rule 1: load the DB row + visibility.
    let document = state.find_by_storage_key(&key);
    // The scope: the host tenant when it resolves, else the scope
    // embedded in a signed token.
    let host_tenant = require_tenant(&req).ok();
    let token_claim = query
        .token
        .as_deref()
        .and_then(|token| state.verify_signed_token(token).ok());
    let scope = match (&host_tenant, &token_claim) {
        (Some(tenant), _) => Some(StorageScope::Tenant(tenant.tenant_id)),
        (None, Some((_, scope))) => Some(scope.clone()),
        (None, None) => None,
    };
    // A token is valid for this file only when it names this
    // document and (when a host tenant resolved) that tenant's
    // scope — a token for another tenant cannot replay here.
    let token_ok = token_grants_access(
        &token_claim,
        &document,
        host_tenant.as_ref().map(|tenant| tenant.tenant_id),
    );
    // Rules 1 + 2: public skips the header check; private needs
    // header auth or a valid token. The header actor is kept for
    // the download log (a public document served without auth
    // logs as "anonymous"; a token redemption as "signed-url").
    let header_actor = match &document {
        Some(document) if document.visibility == "public" => None,
        Some(document) => private_caller_actor(&state, &ctx, &req, document),
        None => None,
    };
    let authorized = match &document {
        Some(document) if document.visibility == "public" => true,
        Some(_) => token_ok || header_actor.is_some(),
        None => false,
    };
    if !private_access_decision(document.as_ref(), authorized) {
        return xml_not_found();
    }
    let document = match document {
        Some(document) => document,
        None => return xml_not_found(),
    };
    let scope = match scope {
        Some(scope) => scope,
        // A public document with no resolvable scope cannot be
        // located (the key is scope-free): uniform 404.
        None => return xml_not_found(),
    };
    let area = StorageArea::from_visibility(&document.visibility);
    let provider_key = scope.provider_key_for_area(area, &key);
    let provider = match state.provider() {
        Ok(provider) => provider,
        Err(_) => return xml_not_found(),
    };
    // Rule 4: the authoritative size comes from the provider (an
    // old version's key has an old size); a missing object is a
    // uniform 404.
    let total = match provider.object_size(&provider_key).await {
        Ok(total) => total,
        Err(_) => return xml_not_found(),
    };
    let download_name = safe_download_name(&document.name);
    let range = parse_range(
        req.headers()
            .get(actix_web::http::header::RANGE)
            .and_then(|value| value.to_str().ok()),
        total,
    );
    let cache_control = if document.visibility == "public" {
        "public, max-age=31536000, immutable"
    } else {
        "private, no-store"
    };
    let log_actor = match &header_actor {
        Some(actor) => actor.clone(),
        None => {
            if token_ok {
                "signed-url".to_string()
            } else {
                "anonymous".to_string()
            }
        }
    };
    let response = serve_object(
        &state,
        &provider_key,
        &document.mime,
        total,
        Some(&download_name),
        query.wants_download(),
        cache_control,
        range,
        xml_not_found(),
    )
    .await;
    // Log the download only when bytes are actually served (a
    // 206/200 response); a 416 is not a download.
    if response.status().is_success() {
        state.log_download(&document, &log_actor, "files");
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_document(visibility: &str) -> DocumentRecord {
        DocumentRecord {
            id: "doc-1".to_string(),
            name: "report.pdf".to_string(),
            title: "report.pdf".to_string(),
            alt_text: None,
            caption: None,
            description: None,
            custom_metadata: serde_json::Map::new(),
            module_id: "documents".to_string(),
            company_id: "company-1".to_string(),
            owner_id: "user-1".to_string(),
            scope: "owner".to_string(),
            mime: "application/pdf".to_string(),
            size: 100,
            checksum: Some("aa".repeat(32)),
            storage_key: "documents/2026/11/23/019a1234-5678-7abc-def0-123456789abc.pdf"
                .to_string(),
            current_version: 1,
            version_count: 1,
            scan_status: "clean".to_string(),
            scan_threat: None,
            visibility: visibility.to_string(),
            created_at: now(),
            updated_at: now(),
        }
    }

    #[test]
    fn private_access_decision_covers_the_four_rules() {
        let public = test_document("public");
        let private = test_document("private");
        // Rule 1: a public document serves with NO header check.
        assert!(private_access_decision(Some(&public), false));
        // Rule 2: a private document serves only with a valid
        // header (or token); an invalid one is a uniform 404.
        assert!(private_access_decision(Some(&private), true));
        assert!(!private_access_decision(Some(&private), false));
        // Rule 4: no row at all is a 404 even with valid auth.
        assert!(!private_access_decision(None, true));
        assert!(!private_access_decision(None, false));
    }

    #[test]
    fn token_grants_access_requires_this_document_and_this_tenant() {
        let document = test_document("private");
        let tenant_a = uuid::Uuid::new_v4();
        let tenant_b = uuid::Uuid::new_v4();
        let claim = |doc_id: &str, scope: StorageScope| Some((doc_id.to_string(), scope));
        // Same document, same tenant: grants.
        assert!(token_grants_access(
            &claim("doc-1", StorageScope::Tenant(tenant_a)),
            &Some(document.clone()),
            Some(tenant_a),
        ));
        // Same document, another tenant's token: no replay.
        assert!(!token_grants_access(
            &claim("doc-1", StorageScope::Tenant(tenant_b)),
            &Some(document.clone()),
            Some(tenant_a),
        ));
        // Another document's token: no replay.
        assert!(!token_grants_access(
            &claim("doc-2", StorageScope::Tenant(tenant_a)),
            &Some(document.clone()),
            Some(tenant_a),
        ));
        // No host tenant (token-only redemption): the token's own
        // scope is the authority, so the same document grants.
        assert!(token_grants_access(
            &claim("doc-1", StorageScope::Tenant(tenant_b)),
            &Some(document.clone()),
            None,
        ));
        // No token, or no document: never grants.
        assert!(!token_grants_access(
            &None,
            &Some(document.clone()),
            Some(tenant_a),
        ));
        assert!(!token_grants_access(
            &claim("doc-1", StorageScope::Tenant(tenant_a)),
            &None,
            Some(tenant_a),
        ));
    }

    #[test]
    fn parse_range_covers_the_supported_forms() {
        // No header: the whole object.
        assert_eq!(parse_range(None, 1000), RangeRequest::Full);
        // Closed, open, and suffix ranges.
        assert_eq!(parse_range(Some("bytes=0-99"), 1000), RangeRequest::Range(0, 99));
        assert_eq!(
            parse_range(Some("bytes=500-"), 1000),
            RangeRequest::Range(500, 999)
        );
        assert_eq!(
            parse_range(Some("bytes=-100"), 1000),
            RangeRequest::Range(900, 999)
        );
        // An end past the object is clamped to the last byte.
        assert_eq!(
            parse_range(Some("bytes=900-2000"), 1000),
            RangeRequest::Range(900, 999)
        );
        // Invalid forms are all unsatisfiable (416).
        assert_eq!(parse_range(Some("bytes=500-100"), 1000), RangeRequest::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=1000-"), 1000), RangeRequest::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=2000-3000"), 1000), RangeRequest::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=0-99,200-299"), 1000), RangeRequest::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=-0"), 1000), RangeRequest::Unsatisfiable);
        assert_eq!(parse_range(Some("items=0-99"), 1000), RangeRequest::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=abc-def"), 1000), RangeRequest::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=0-99"), 0), RangeRequest::Unsatisfiable);
    }
}
