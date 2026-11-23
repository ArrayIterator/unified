// documents — Validation helpers and small pure utilities.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::modules::Module;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::HttpRequest;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use std::path::Path;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}

pub(crate) fn paginate<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

pub(crate) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

/// Strip any directory components from an uploaded name. Returns an
/// error when nothing usable remains (for example `..` or `/`).
pub(crate) fn sanitize_file_name(raw: &str) -> UnifiedResult<String> {
    let name = Path::new(raw)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if name.is_empty() || name == "." || name == ".." {
        return Err(UnifiedError::bad_request(
            "Uploaded file name is not usable",
        ));
    }
    Ok(name)
}

pub(crate) fn mime_for(name: &str, bytes: &[u8]) -> String {
    if let Some(kind) = infer::get(bytes) {
        return kind.mime_type().to_string();
    }
    match Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "txt" => "text/plain".to_string(),
        "html" | "htm" => "text/html".to_string(),
        "css" => "text/css".to_string(),
        "js" | "mjs" => "text/javascript".to_string(),
        "json" => "application/json".to_string(),
        "pdf" => "application/pdf".to_string(),
        "zip" => "application/zip".to_string(),
        "md" => "text/markdown".to_string(),
        "csv" => "text/csv".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

/// Image dimensions from the image header bytes only — no rendering.
/// Supports PNG, GIF, JPEG (first SOF marker), and WebP (VP8X / VP8
/// lossy / VP8L). Returns `None` for anything else.
pub(crate) fn image_dimensions(mime: &str, bytes: &[u8]) -> Option<(u32, u32)> {
    match mime {
        "image/png" => {
            if bytes.len() >= 24 && &bytes[..8] == b"\x89PNG\r\n\x1a\n" {
                let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
                let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
                Some((width, height))
            } else {
                None
            }
        }
        "image/gif" => {
            if bytes.len() >= 10 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a") {
                let width = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
                let height = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
                Some((width, height))
            } else {
                None
            }
        }
        "image/jpeg" => jpeg_dimensions(bytes),
        "image/webp" => {
            if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
                webp_dimensions(bytes)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut index = 2usize;
    while index + 9 <= bytes.len() {
        if bytes[index] != 0xFF {
            return None;
        }
        let marker = bytes[index + 1];
        // SOF0 / SOF1 / SOF2 (skip DHT, JPGn, and restart markers).
        if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC {
            let height = u16::from_be_bytes([bytes[index + 5], bytes[index + 6]]) as u32;
            let width = u16::from_be_bytes([bytes[index + 7], bytes[index + 8]]) as u32;
            return Some((width, height));
        }
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
            index += 2;
            continue;
        }
        let length = u16::from_be_bytes([bytes[index + 2], bytes[index + 3]]) as usize;
        if length < 2 {
            return None;
        }
        index += 2 + length;
    }
    None
}

fn webp_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 30 {
        return None;
    }
    match &bytes[12..16] {
        b"VP8X" => {
            let width = (u32::from_le_bytes([bytes[24], bytes[25], bytes[26], 0]) + 1) as u32;
            let height = (u32::from_le_bytes([bytes[27], bytes[28], bytes[29], 0]) + 1) as u32;
            Some((width, height))
        }
        b"VP8 " => {
            // Lossy VP8: 14-bit width / height at offset 26 / 28.
            let width = (u16::from_le_bytes([bytes[26], bytes[27]]) & 0x3FFF) as u32;
            let height = (u16::from_le_bytes([bytes[28], bytes[29]]) & 0x3FFF) as u32;
            Some((width, height))
        }
        b"VP8L" => {
            let packed = u32::from_le_bytes([bytes[21], bytes[22], bytes[23], bytes[24]]);
            let width = (packed & 0x3FFF) + 1;
            let height = ((packed >> 14) & 0x3FFF) + 1;
            Some((width, height))
        }
        _ => None,
    }
}

/// Constant-time equality for HMAC tag comparison (lengths must
/// match; the fold runs over every byte either way).
pub(crate) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for index in 0..left.len() {
        diff |= left[index] ^ right[index];
    }
    diff == 0
}

pub(crate) fn is_valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit())
}

pub(crate) fn valid_module_id(value: &str) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return Err(UnifiedError::bad_request(
            "Module id must be 1..=128 characters",
        ));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(UnifiedError::bad_request(
            "Module id must contain only lowercase letters, digits, '_' or '-'",
        ));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn valid_scope(value: &str) -> UnifiedResult<String> {
    match value.trim() {
        "owner" | "company" | "module" => Ok(value.trim().to_string()),
        _ => Err(UnifiedError::bad_request(
            "Scope must be one of: owner, company, module",
        )),
    }
}

/// Validate a `visibility` value (`public` or `private`). The
/// enum leaves room for a future `unlisted` value; until it
/// exists, anything else is rejected.
pub(crate) fn valid_visibility(value: &str) -> UnifiedResult<String> {
    match value.trim() {
        "public" | "private" => Ok(value.trim().to_string()),
        _ => Err(UnifiedError::bad_request(
            "Visibility must be one of: public, private",
        )),
    }
}

/// Maximum bytes accumulated for one multipart text field
/// (`module_id`, `company_id`, `scope`, `part_index`, `checksum`).
/// Text fields are short identifiers; without a cap a client could
/// stream an unbounded field into memory before any validation
/// runs. 4 KiB is far above any legitimate value (ids are capped
/// at 128 characters elsewhere).
pub(crate) const MAX_MULTIPART_TEXT_BYTES: usize = 4096;

/// Append one multipart chunk to an accumulated text field,
/// rejecting the field when it would exceed
/// [`MAX_MULTIPART_TEXT_BYTES`].
pub(crate) fn push_multipart_text(text: &mut String, chunk: &[u8]) -> UnifiedResult<()> {
    if text.len() + chunk.len() > MAX_MULTIPART_TEXT_BYTES {
        return Err(UnifiedError::bad_request(
            "Multipart text field exceeds the 4 KiB limit",
        ));
    }
    text.push_str(&String::from_utf8_lossy(chunk));
    Ok(())
}

/// Maximum length of a company id, matching the 128-character cap
/// enforced on `company_id` inputs elsewhere in this module.
pub(crate) const MAX_COMPANY_ID_LEN: usize = 128;

/// The caller's company id, resolved server-side — never taken
/// from a request body, a query filter, or the `X-Company-Id`
/// header. The verified caller's server-resolved company first,
/// then the `user.company_requested` hook answered by the users
/// module. The header is NEVER read here: it is client-asserted,
/// and trusting it would let any JWT caller read another company's
/// quota usage. `None` when neither resolves.
pub(crate) fn resolve_caller_company_id(
    ctx: &AppContext,
    req: &HttpRequest,
    caller: &Caller,
) -> Option<String> {
    if let Some(raw) = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty() && value.len() <= MAX_COMPANY_ID_LEN)
    {
        return Some(raw);
    }
    let hook = ctx
        .hooks
        .emit("user.company_requested", json!({ "user_id": caller.actor }));
    hook.get("company_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= MAX_COMPANY_ID_LEN)
        .map(str::to_string)
}

/// True when `caller` may read quota usage for `company_id`:
/// admins may read any company; a non-admin may read only their
/// own company (as resolved by [`resolve_caller_company_id`]) or
/// a company they demonstrably belong to because they own a
/// document in it. A caller with no resolvable company and no
/// owned document in the company is denied (fail closed): unlike
/// some read paths, an unresolvable company is NOT treated as a
/// platform caller here, because quota usage is company-scoped
/// data. An empty `company_id` requests no company scope, so it
/// is always allowed (the user scope is checked separately).
pub(crate) fn may_view_company_quota(
    is_admin: bool,
    caller_company: Option<&str>,
    company_id: &str,
    owns_document_in_company: bool,
) -> bool {
    if company_id.is_empty() {
        return true;
    }
    if is_admin {
        return true;
    }
    if caller_company == Some(company_id) {
        return true;
    }
    owns_document_in_company
}

/// `X-User-Id` identifies the actor, `X-Role` carries the role slug
/// the capability check runs against.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Resolve the tenant for this request, failing closed when the
/// tenant-resolution middleware attached no tenant context (401)
/// or the resolved tenant id is not a uuidv7 UUID (403). Every
/// tenant-data handler in this module calls this FIRST, before
/// caller identity or any capability check, so an unresolved
/// request never reaches a store or an emit. There is no fallback
/// to the `public` schema and no default tenant. See
/// `unified_core::tenancy` (`TENANT_SCOPE_CONTRACT`).
///
/// Note: document records carry `company_id: String` (empty when
/// unscoped), not `Option<String>`, so the scope helpers below
/// take the record company as `&str`.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// — a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources.
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company))
            if tenant_company != verified_company =>
        {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified.clone()),
    }
}

/// Verify a request body's `company_id` against the caller's
/// company scope and return the company id to store. Fails closed
/// (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known (neither the tenant context nor
/// the verified caller carries a company), a body company is refused:
/// an unresolved caller may not claim a company (the shared
/// `CallerScope::Unresolved` rule); omitting the company stores
/// a global row.
pub(crate) fn require_company_match(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
    body_company_id: &Option<String>,
) -> UnifiedResult<Option<String>> {
    let caller = caller_company(tenant, req)?;
    match (body_company_id, caller) {
        (Some(body), Some(caller_company)) if body != caller_company => Err(
            UnifiedError::forbidden("company_id does not match the caller's company scope"),
        ),
        (Some(body), Some(_)) => Ok(Some(body.clone())),
        (Some(body), None) => Err(UnifiedError::forbidden(
            "company_id does not match the caller's company scope",
        )),
        (None, caller) => Ok(caller),
    }
}

/// `require_company_match` against an already-resolved caller
/// scope, for handlers that computed `caller_company` first (or,
/// like the multipart upload handler, run inside an `async move`
/// block that cannot borrow the request).
pub(crate) fn require_company_match_for(
    caller: &Option<String>,
    body_company_id: &Option<String>,
) -> UnifiedResult<Option<String>> {
    match (body_company_id, caller) {
        (Some(body), Some(caller_company)) if body != caller_company => Err(
            UnifiedError::forbidden("company_id does not match the caller's company scope"),
        ),
        (Some(body), Some(_)) => Ok(Some(body.clone())),
        (Some(body), None) => Err(UnifiedError::forbidden(
            "company_id does not match the caller's company scope",
        )),
        (None, caller) => Ok(caller.clone()),
    }
}

/// True when a stored record belongs to the caller's company
/// scope. With no caller scope (an unresolved caller) only
/// global records match: unscoped records are visible, every
/// company-owned record is denied (the shared
/// `CallerScope::Unresolved` rule). With a scope, only records carrying
/// exactly that company match: unscoped (legacy) records are
/// hidden from scoped callers rather than leaked across companies.
pub(crate) fn in_company_scope(record_company_id: &str, caller_company: &Option<String>) -> bool {
    match caller_company {
        None => record_company_id.is_empty(),
        Some(company) => record_company_id == company.as_str(),
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope. Single-record reads and mutations call this
/// right after loading the record; list handlers use
/// `in_company_scope` as a filter instead.
pub(crate) fn require_record_in_company_scope(
    record_company_id: &str,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    if in_company_scope(record_company_id, caller_company) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "The record belongs to a different company",
        ))
    }
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// Authorize one request: caller identity + one capability. Returns
/// the caller on success so handlers can use the actor id.
pub(crate) fn authorize(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<Caller> {
    let found = caller(req)?;
    require_permission(ctx, &found.role, permission)?;
    Ok(found)
}

/// Escape a file name for the `Content-Disposition` header: strip
/// quotes, backslashes, and control characters.
pub(crate) fn safe_download_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control() && *c != '"' && *c != '\\')
        .collect()
}

pub(crate) fn validate_extensions(raw: &[String]) -> UnifiedResult<Vec<String>> {
    let mut cleaned: Vec<String> = Vec::new();
    for value in raw {
        let extension = value.trim().trim_start_matches('.').to_lowercase();
        if extension.is_empty() || extension.len() > 16 {
            return Err(UnifiedError::bad_request(
                "Extensions must be 1..=16 characters",
            ));
        }
        if !extension
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(UnifiedError::bad_request(
                "Extensions must contain only letters, digits, or '-'",
            ));
        }
        if !cleaned.contains(&extension) {
            cleaned.push(extension);
        }
    }
    if cleaned.is_empty() {
        return Err(UnifiedError::bad_request(
            "At least one allowed extension is required",
        ));
    }
    Ok(cleaned)
}

pub(crate) fn validate_mime_types(raw: &[String]) -> UnifiedResult<Vec<String>> {
    let mut cleaned: Vec<String> = Vec::new();
    for value in raw {
        let mime = value.trim().to_lowercase();
        if mime.is_empty() || mime.len() > 128 || !mime.contains('/') {
            return Err(UnifiedError::bad_request(
                "MIME types must look like \"type/subtype\"",
            ));
        }
        if !cleaned.contains(&mime) {
            cleaned.push(mime);
        }
    }
    Ok(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tenant context the tenant-resolution middleware would
    /// attach for a request on a tenant host
    /// (`unified_core::middleware::TenantContext`, the exact struct
    /// `tenancy::TenantContext::from_http_request` reads from the
    /// request extensions). The company is empty, so the resolved
    /// scope is unscoped.
    fn tenant_request() -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: String::new(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            },
        );
        req
    }

        /// A request with a tenant context carrying `tenant_company`
    /// (empty = no tenant company), a verified caller carrying
    /// `verified_company` (empty = no company resolved), and a
    /// forged `X-Company-Id` header when `forged_header` is
    /// non-empty. The header must never affect the scope.
    fn scoped_request(
        tenant_company: &str,
        verified_company: &str,
        forged_header: &str,
    ) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let mut req = actix_web::test::TestRequest::default();
        if !forged_header.is_empty() {
            req = req.insert_header(("x-company-id", forged_header));
        }
        let req = req.to_http_request();
        if !verified_company.is_empty() {
            req.extensions_mut().insert(
                unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
                    .with_company(verified_company.to_string()),
            );
        }
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: tenant_company.to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req
    }

    #[test]
    fn caller_company_prefers_tenant_company_over_verified_caller() {
        let req = scoped_request("company-a", "company-a", "");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            super::caller_company(&tenant, &req).unwrap(),
            Some("company-a".to_string())
        );
    }

    #[test]
    fn caller_company_uses_verified_caller_when_tenant_has_none() {
        let req = scoped_request("", "company-b", "");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            super::caller_company(&tenant, &req).unwrap(),
            Some("company-b".to_string())
        );
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = scoped_request("company-a", "company-b", "");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert!(super::caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn resolve_caller_company_id_prefers_the_verified_caller_over_a_forged_header() {
        use std::sync::Arc;
        use unified_core::config::AppConfig;
        use unified_core::hooks::HookBus;
        use unified_core::modules::ModuleRegistry;
        let ctx = AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        );
        let caller = Caller { actor: "user-1".to_string(), role: "member".to_string() };
        let req = {
            use actix_web::HttpMessage;
            let req = actix_web::test::TestRequest::default()
                .insert_header(("x-company-id", "company-forged"))
                .to_http_request();
            req.extensions_mut().insert(
                unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
                    .with_company("company-a".to_string()),
            );
            req
        };
        assert_eq!(
            super::resolve_caller_company_id(&ctx, &req, &caller),
            Some("company-a".to_string())
        );
        let bare = actix_web::test::TestRequest::default()
            .insert_header(("x-company-id", "company-forged"))
            .to_http_request();
        assert_eq!(
            super::resolve_caller_company_id(&ctx, &bare, &caller),
            None
        );
    }

    #[test]
    fn caller_company_ignores_a_forged_company_header() {
        let req = scoped_request("", "", "company-forged");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(super::caller_company(&tenant, &req).unwrap(), None);
    }

#[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request();
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
        assert_eq!(caller_company(&tenant, &req).unwrap(), None);
    }

    #[test]
    fn in_company_scope_hides_unscoped_records_from_scoped_callers() {
        assert!(in_company_scope("c1", &Some("c1".to_string())));
        assert!(!in_company_scope("c2", &Some("c1".to_string())));
        assert!(!in_company_scope("", &Some("c1".to_string())));
        assert!(in_company_scope("", &None));
        assert!(!in_company_scope("co-1", &None));
    }

    #[test]
    fn require_company_match_for_refuses_body_company_from_unresolved_callers() {
        let body = Some("c1".to_string());
        assert!(require_company_match_for(&None, &body).is_err());
        assert!(require_company_match_for(&None, &None).unwrap().is_none());
        assert_eq!(
            require_company_match_for(&Some("c1".to_string()), &body).unwrap(),
            Some("c1".to_string())
        );
        assert!(
            require_company_match_for(&Some("c1".to_string()), &Some("c2".to_string())).is_err()
        );
    }

    #[test]
    fn sanitize_file_name_strips_directories() {
        assert_eq!(sanitize_file_name("/a/b/report.pdf").unwrap(), "report.pdf");
        assert_eq!(sanitize_file_name("report.pdf").unwrap(), "report.pdf");
        assert_eq!(sanitize_file_name("  spaced.txt  ").unwrap(), "spaced.txt");
        assert!(sanitize_file_name("..").is_err());
        assert!(sanitize_file_name("/").is_err());
        assert!(sanitize_file_name("").is_err());
    }

    #[test]
    fn mime_for_extension_fallback() {
        assert_eq!(mime_for("notes.txt", &[]), "text/plain");
        assert_eq!(mime_for("doc.pdf", &[]), "application/pdf");
        assert_eq!(mime_for("page.html", &[]), "text/html");
        assert_eq!(mime_for("data.json", &[]), "application/json");
        assert_eq!(mime_for("x.bin", &[]), "application/octet-stream");
        assert_eq!(mime_for("noext", &[]), "application/octet-stream");
    }

    #[test]
    fn mime_for_detects_png_magic() {
        let png = [
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H',
            b'D', b'R', 0, 0, 3, 0x20, 0, 0, 2, 0x58,
        ];
        assert_eq!(mime_for("x.png", &png), "image/png");
    }

    #[test]
    fn image_dimensions_png_and_gif() {
        let png = [
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H',
            b'D', b'R', 0, 0, 3, 0x20, 0, 0, 2, 0x58,
        ];
        assert_eq!(image_dimensions("image/png", &png), Some((800, 600)));
        assert_eq!(image_dimensions("image/png", b"short"), None);
        let gif = *b"GIF89a\x20\x03\x58\x02";
        assert_eq!(image_dimensions("image/gif", &gif), Some((800, 600)));
        let gif87 = *b"GIF87a\x20\x03\x58\x02";
        assert_eq!(image_dimensions("image/gif", &gif87), Some((800, 600)));
        assert_eq!(image_dimensions("image/bmp", &gif), None);
    }

    #[test]
    fn jpeg_dimensions_reads_sof0() {
        let jpeg = [
            0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 0x64, 0x00, 0xC8, 0, 0,
            0, 0, 0, 0,
        ];
        assert_eq!(image_dimensions("image/jpeg", &jpeg), Some((200, 100)));
        assert_eq!(jpeg_dimensions(&[]), None);
        assert_eq!(jpeg_dimensions(&[0xFF, 0xD8]), None);
    }

    #[test]
    fn webp_dimensions_all_chunks() {
        let mut vp8x = [0u8; 30];
        vp8x[0..4].copy_from_slice(b"RIFF");
        vp8x[8..12].copy_from_slice(b"WEBP");
        vp8x[12..16].copy_from_slice(b"VP8X");
        vp8x[24] = 99;
        vp8x[27] = 49;
        assert_eq!(image_dimensions("image/webp", &vp8x), Some((100, 50)));
        let mut vp8 = [0u8; 30];
        vp8[0..4].copy_from_slice(b"RIFF");
        vp8[8..12].copy_from_slice(b"WEBP");
        vp8[12..16].copy_from_slice(b"VP8 ");
        vp8[26] = 100;
        vp8[28] = 50;
        assert_eq!(image_dimensions("image/webp", &vp8), Some((100, 50)));
        let mut vp8l = [0u8; 30];
        vp8l[0..4].copy_from_slice(b"RIFF");
        vp8l[8..12].copy_from_slice(b"WEBP");
        vp8l[12..16].copy_from_slice(b"VP8L");
        vp8l[21] = 0x63;
        vp8l[22] = 0x40;
        vp8l[23] = 0x0C;
        vp8l[24] = 0x00;
        assert_eq!(image_dimensions("image/webp", &vp8l), Some((100, 50)));
        assert_eq!(webp_dimensions(&[0u8; 10]), None);
    }

    #[test]
    fn constant_time_eq_behaviour() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(constant_time_eq(b"", b""));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn is_valid_sha256_shape() {
        assert!(is_valid_sha256(&"a".repeat(64)));
        assert!(is_valid_sha256(&"A".repeat(64)));
        assert!(is_valid_sha256(&"0123456789abcdef".repeat(4)));
        assert!(!is_valid_sha256(&"a".repeat(63)));
        assert!(!is_valid_sha256(&"g".repeat(64)));
    }

    #[test]
    fn valid_module_id_rules() {
        assert_eq!(valid_module_id("timetracker").unwrap(), "timetracker");
        assert_eq!(valid_module_id("a-b_c9").unwrap(), "a-b_c9");
        assert!(valid_module_id("").is_err());
        assert!(valid_module_id("Time").is_err());
        assert!(valid_module_id("a b").is_err());
        assert!(valid_module_id(&"a".repeat(129)).is_err());
    }

    #[test]
    fn require_text_trims_and_limits() {
        assert_eq!(require_text(" x ", "name", 10).unwrap(), "x");
        assert!(require_text("", "name", 10).is_err());
        assert!(require_text("   ", "name", 10).is_err());
        assert!(require_text(&"x".repeat(11), "name", 10).is_err());
    }

    #[test]
    fn valid_scope_values() {
        assert_eq!(valid_scope("owner").unwrap(), "owner");
        assert_eq!(valid_scope("company").unwrap(), "company");
        assert_eq!(valid_scope("module").unwrap(), "module");
        assert!(valid_scope("admin").is_err());
        assert!(valid_scope("").is_err());
    }

    #[test]
    fn validate_extensions_normalises_and_dedupes() {
        assert_eq!(
            validate_extensions(&[
                "PDF".to_string(),
                ".pdf".to_string(),
                "png".to_string()
            ])
            .unwrap(),
            vec!["pdf".to_string(), "png".to_string()]
        );
        assert!(validate_extensions(&[]).is_err());
        assert!(validate_extensions(&["a b".to_string()]).is_err());
        assert!(validate_extensions(&["x".repeat(17)]).is_err());
        assert!(validate_extensions(&["".to_string()]).is_err());
    }

    #[test]
    fn validate_mime_types_normalises() {
        assert_eq!(
            validate_mime_types(&[
                "Application/JSON".to_string(),
                "application/json".to_string()
            ])
            .unwrap(),
            vec!["application/json".to_string()]
        );
        assert!(validate_mime_types(&["nope".to_string()]).is_err());
        assert!(validate_mime_types(&["".to_string()]).is_err());
        assert_eq!(validate_mime_types(&[]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn safe_download_name_strips_dangerous_chars() {
        assert_eq!(safe_download_name("a\"b\\c"), "abc");
        assert_eq!(safe_download_name("plain.txt"), "plain.txt");
        assert_eq!(safe_download_name("a\x01b\x7fc"), "abc");
    }

    #[test]
    fn jpeg_dimensions_sof_ending_exactly_at_eof() {
        // SOI (2 bytes) + SOF0 segment whose 9 bytes end exactly
        // at EOF: the scan loop needs `index + 9 <= len`, not `<`.
        let jpeg = [
            0xFF, 0xD8, // SOI
            0xFF, 0xC0, // SOF0 marker
            0x00, 0x11, // segment length
            0x08, // precision
            0x00, 0x64, // height 100
            0x00, 0xC8, // width 200
        ];
        assert_eq!(jpeg.len(), 11);
        assert_eq!(jpeg_dimensions(&jpeg), Some((200, 100)));
        assert_eq!(image_dimensions("image/jpeg", &jpeg), Some((200, 100)));
        // One byte short of the SOF payload still yields nothing.
        assert_eq!(jpeg_dimensions(&jpeg[..10]), None);
    }

    #[test]
    fn push_multipart_text_enforces_4kib_cap() {
        let mut text = String::new();
        push_multipart_text(&mut text, b"hello").unwrap();
        assert_eq!(text, "hello");
        push_multipart_text(&mut text, &vec![b'x'; MAX_MULTIPART_TEXT_BYTES - 5]).unwrap();
        assert_eq!(text.len(), MAX_MULTIPART_TEXT_BYTES);
        // One more byte over the cap is rejected and leaves the
        // accumulated text unchanged.
        assert!(push_multipart_text(&mut text, b"x").is_err());
        assert_eq!(text.len(), MAX_MULTIPART_TEXT_BYTES);
        // A single chunk larger than the cap is rejected outright.
        let mut other = String::new();
        assert!(push_multipart_text(&mut other, &vec![b'y'; MAX_MULTIPART_TEXT_BYTES + 1])
            .is_err());
        assert!(other.is_empty());
    }

    #[test]
    fn may_view_company_quota_rules() {
        // Admins may read any company.
        assert!(may_view_company_quota(true, None, "company-a", false));
        assert!(may_view_company_quota(true, Some("company-b"), "company-a", false));
        // A caller may read their own company.
        assert!(may_view_company_quota(
            false,
            Some("company-a"),
            "company-a",
            false
        ));
        // Another company is denied without owned documents there.
        assert!(!may_view_company_quota(
            false,
            Some("company-b"),
            "company-a",
            false
        ));
        // No resolvable company and no owned document: denied
        // (fail closed, not treated as a platform caller).
        assert!(!may_view_company_quota(false, None, "company-a", false));
        // No resolvable company, but the caller owns a document in
        // the company: allowed (local membership evidence).
        assert!(may_view_company_quota(false, None, "company-a", true));
        // No company scope requested: always allowed.
        assert!(may_view_company_quota(false, None, "", false));
    }
}
