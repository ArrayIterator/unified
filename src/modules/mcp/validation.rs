//! mcp - validation.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::apikeys::{authenticate_key, key_has_scope, note_used, AuthenticatedKey};
use actix_web::HttpRequest;
use chrono::Utc;
use serde_json::{json, Map, Value};
use uuid::Uuid;
use super::protocol::*;
use super::catalog::*;
use super::state::*;

/// Permissions enforced on the admin handlers, following the
/// calculator pattern: `mcp:read` for list/get/config reads,
/// `mcp:manage` for create/update/delete/config writes. Checked
/// through the `role.check` hook; no handler answering means not
/// allowed (fail closed).
pub(crate) const PERM_MCP_READ: &str = "mcp:read";
pub(crate) const PERM_MCP_MANAGE: &str = "mcp:manage";

/// The server-verified caller (see `unified_core::caller::Caller`):
/// the user id identifies the actor, the server-resolved role is the
/// role slug or id the capability check runs against.
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

pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// True when `column` is denied: its lower-cased name contains a
/// denylisted fragment anywhere. The check is deliberately a plain
/// substring match, not a word-boundary match: separator-less
/// secret names such as `passwordhash`, `secretkey`, or `authtoken`
/// are denied too, and so is any name containing `key`
/// (`encryption_key`, `signing_key`, `webhook_key`). The accepted
/// trade-off is false positives on innocent names that merely
/// contain a fragment (`tokenized`, `monkey`): fail closed, never
/// leak a credential column because its name lacked a separator.
pub fn column_is_denied(column: &str) -> bool {
    let lower = column.to_lowercase();
    DENYLISTED_COLUMNS
        .iter()
        .any(|fragment| lower.contains(fragment))
}

pub(crate) fn find_table(name: &str) -> Option<&'static ReadOnlyTable> {
    READ_ONLY_TABLES.iter().find(|table| table.name == name)
}

pub(crate) fn find_source(name: &str) -> Option<&'static ContextSource> {
    CONTEXT_SOURCES.iter().find(|source| source.name == name)
}

pub(crate) fn tool_is_known(name: &str) -> bool {
    builtin_tools().iter().any(|tool| tool.name == name)
}

/// The tool list a new server gets when the create request omits
/// `allowed_tools`: every built-in tool, made explicit, so the
/// stored list always says what the server may run. An explicitly
/// empty list is different: it means "no tool" and is fail-closed
/// by `authenticate_mcp_client`.
pub(crate) fn default_allowed_tools() -> Vec<String> {
    builtin_tools()
        .iter()
        .map(|tool| tool.name.to_string())
        .collect()
}

/// Validate a server slug: 3..=64 chars, lower-case letters, digits,
/// and hyphens only, not starting or ending with a hyphen.
pub(crate) fn validate_slug(slug: &str) -> UnifiedResult<String> {
    let slug = slug.trim().to_lowercase();
    if slug.chars().count() < 3 || slug.chars().count() > 64 {
        return Err(UnifiedError::bad_request(
            "slug must be between 3 and 64 characters",
        ));
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return Err(UnifiedError::bad_request(
            "slug must not start or end with '-'",
        ));
    }
    if !slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(UnifiedError::bad_request(
            "slug may only contain lower-case letters, digits, and '-'",
        ));
    }
    Ok(slug)
}

/// Validate a PostgreSQL role name used with `SET LOCAL ROLE`:
/// a bare identifier, letters/digits/underscore, not starting with a
/// digit. The value comes from an admin, never from an MCP client,
/// and it is still validated because it is interpolated into SQL as
/// a quoted identifier.
pub(crate) fn validate_role_name(role: &str) -> UnifiedResult<String> {
    let role = role.trim();
    if role.is_empty() || role.chars().count() > 63 {
        return Err(UnifiedError::bad_request(
            "read_only_role must be a non-empty identifier of at most 63 characters",
        ));
    }
    let mut chars = role.chars();
    let first = chars.next().unwrap_or(' ');
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(UnifiedError::bad_request(
            "read_only_role must start with a letter or '_'",
        ));
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(UnifiedError::bad_request(
            "read_only_role may only contain letters, digits, and '_'",
        ));
    }
    Ok(role.to_string())
}

pub(crate) fn validate_tool_list(tools: &[String]) -> UnifiedResult<Vec<String>> {
    if tools.len() > 64 {
        return Err(UnifiedError::bad_request(
            "allowed_tools must contain at most 64 tools",
        ));
    }
    let mut out: Vec<String> = Vec::new();
    for tool in tools {
        let tool = tool.trim();
        if tool.is_empty() {
            return Err(UnifiedError::bad_request(
                "allowed_tools must not contain an empty tool name",
            ));
        }
        if !tool_is_known(tool) {
            return Err(UnifiedError::bad_request(format!(
                "allowed_tools names unknown tool \"{tool}\""
            ))
            .with_metadata("tool", json!(tool)));
        }
        if !out.contains(&tool.to_string()) {
            out.push(tool.to_string());
        }
    }
    Ok(out)
}

pub(crate) fn validate_rate_limit(limit: u32) -> UnifiedResult<u32> {
    if limit == 0 || limit > 100_000 {
        return Err(UnifiedError::bad_request(
            "rate_limit_per_minute must be between 1 and 100000",
        ));
    }
    Ok(limit)
}

/// Recursively remove every denylisted key from a JSON value, as
/// defence in depth on top of the column allowlist: even if a source
/// module hands back a row that contains a credential field, the MCP
/// server never forwards it to a client.
pub(crate) fn scrub_value(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, entry) in map {
                if column_is_denied(&key) {
                    continue;
                }
                out.insert(key, scrub_value(entry));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(scrub_value).collect()),
        other => other,
    }
}

/// Pagination over already-fetched items, in the standard
/// `PageResult` shape.
pub(crate) fn paginate_values(items: Vec<Value>, params: &PaginationParams) -> PageResult<Value> {
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

/// Read the presented API key from `Authorization: Bearer <key>` or
/// `X-Api-Key`. The key value itself is never logged and never stored.
/// The `Bearer` scheme is matched case-insensitively, as RFC 6750
/// requires schemes to be compared that way, so `bearer <key>` and
/// `BEARER <key>` authenticate exactly like `Bearer <key>`.
pub(crate) fn presented_key(req: &HttpRequest) -> Option<String> {
    if let Some(header) = req.headers().get("authorization") {
        if let Ok(text) = header.to_str() {
            let mut parts = text.splitn(2, ' ');
            if let (Some(scheme), Some(key)) = (parts.next(), parts.next()) {
                if scheme.eq_ignore_ascii_case("bearer") {
                    let key = key.trim();
                    if !key.is_empty() {
                        return Some(key.to_string());
                    }
                }
            }
        }
    }
    req.headers()
        .get("x-api-key")
        .and_then(|header| header.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Authenticate an MCP client for one server and one tool.
///
/// Checks, in order: the server is active; a key was presented; the
/// key verifies through the API Keys module; a key with an `owner_id`
/// belongs to the server's owner; the key's scopes grant the tool
/// (`mcp:<tool>`, `mcp:*`, or `*`, via `key_has_scope`); and, for a
/// real tool (not the protocol-level `"read"` check), the server's
/// own `allowed_tools` list permits the tool. An empty
/// `allowed_tools` list permits nothing (fail closed): a server
/// created without an explicit list gets the full built-in list
/// stored explicitly instead of an empty list. The per-key per-minute
/// rate limit (the lower of the key tier and the server limit) is
/// enforced last. On success the key usage is recorded.
///
/// A key scoped only to `mcp:read` may run the protocol methods
/// (`initialize`, `tools/list`, `resources/list`, ...) but may not
/// call any tool: `tools/call` always needs the exact `mcp:<tool>`
/// scope.
pub(crate) fn authenticate_mcp_client(
    ctx: &AppContext,
    state: &McpState,
    req: &HttpRequest,
    server: &McpServer,
    tool: &str,
) -> UnifiedResult<AuthenticatedKey> {
    if server.status != "active" {
        return Err(UnifiedError::forbidden(format!(
            "MCP server \"{}\" is disabled",
            server.slug
        )));
    }
    let presented = presented_key(req)
        .ok_or_else(|| UnifiedError::unauthorized("Missing API key: send Authorization: Bearer <key> or X-Api-Key"))?;
    let authenticated = authenticate_key(&presented)?;
    if let Some(owner_id) = authenticated.owner_id {
        if owner_id != server.owner_id {
            return Err(UnifiedError::forbidden(
                "API key belongs to a different owner than this MCP server",
            ));
        }
    }
    if !key_has_scope(&authenticated.scopes, "mcp", tool) {
        return Err(UnifiedError::forbidden(format!(
            "API key is not scoped to MCP tool \"{tool}\"; grant scope \"mcp:{tool}\" or \"mcp:*\""
        ))
        .with_metadata("tool", json!(tool))
        .with_metadata("scopes", json!(authenticated.scopes)));
    }
    if tool != "read"
        && !server.allowed_tools.iter().any(|entry| entry == tool)
    {
        return Err(UnifiedError::forbidden(format!(
            "MCP server \"{}\" does not allow tool \"{tool}\"",
            server.slug
        ))
        .with_metadata("tool", json!(tool)));
    }
    let limit = authenticated
        .requests_per_minute
        .min(server.rate_limit_per_minute);
    check_rate_limit(state, &authenticated.id, limit)?;
    note_used(authenticated.id);
    ctx.hooks.emit(
        mcp_events::CLIENT_AUTHENTICATED,
        json!({
            "server_id": server.id,
            "key_id": authenticated.id,
            "tool": tool,
        }),
    );
    Ok(authenticated)
}

/// Per-key per-minute counter. The limit value comes from the key's
/// own tier (API Keys module) capped by the server limit; this
/// function only counts, it invents no tier.
pub(crate) fn check_rate_limit(state: &McpState, key_id: &Uuid, limit: u32) -> UnifiedResult<()> {
    let minute = (Utc::now().timestamp() / 60) as u64;
    let bucket_key = key_id.to_string();
    let mut bucket = state
        .rate_buckets
        .entry(bucket_key)
        .or_insert(RateBucket { minute, count: 0 });
    if bucket.minute != minute {
        bucket.minute = minute;
        bucket.count = 0;
    }
    if bucket.count >= limit {
        return Err(UnifiedError::new(
            "Rate limit exceeded for this API key",
            429,
        )
        .with_metadata("limit_per_minute", json!(limit))
        .with_metadata("retry_after_seconds", json!(60 - (Utc::now().timestamp() % 60))));
    }
    bucket.count += 1;
    Ok(())
}

/// Ask Product Management whether `user_id` is entitled to
/// `product_id`, through the `product.entitlement.check` hook event
/// (`{"product_id", "user_id"}` in, `{"entitled": bool}` out).
/// Fail-closed, like the meetings module's join check: no handler
/// answering is a 503, an explicit `{"entitled": false}` (or an
/// `error` in the result) is a 403.
pub(crate) fn check_entitlement(ctx: &AppContext, product_id: &str, user_id: &str) -> UnifiedResult<()> {
    let answer = ctx.hooks.emit(
        "product.entitlement.check",
        json!({ "product_id": product_id, "user_id": user_id }),
    );
    match answer.results.first() {
        Some(result) if result.get("entitled").and_then(Value::as_bool) == Some(true) => Ok(()),
        Some(result) if result.get("error").is_some() => {
            let reason = result
                .get("error")
                .and_then(|value| value.as_str())
                .unwrap_or("entitlement check failed");
            Err(UnifiedError::new(
                format!("Entitlement check failed: {reason}"),
                502,
            ))
        }
        Some(_) => Err(UnifiedError::forbidden(
            "A product entitlement is required for this MCP server",
        )
        .with_metadata("product_id", json!(product_id))),
        None => Err(UnifiedError::new(
            "Entitlement service did not answer product.entitlement.check: no handler is registered",
            503,
        )),
    }
}

pub(crate) fn resolve_server(state: &McpState, id_or_slug: &str) -> UnifiedResult<McpServer> {
    if let Ok(id) = Uuid::parse_str(id_or_slug) {
        if let Some(server) = state.servers.get(&id) {
            return Ok(server.value().clone());
        }
    }
    let slug = id_or_slug.trim().to_lowercase();
    if let Some(id) = state.servers_by_slug.get(&slug) {
        if let Some(server) = state.servers.get(id.value()) {
            return Ok(server.value().clone());
        }
    }
    Err(UnifiedError::not_found(format!(
        "MCP server \"{id_or_slug}\" not found"
    )))
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 3, task 314, group C).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller()` or any capability
/// check.
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

/// Resolve the caller's company for a WRITE (C3 task 315, strict by
/// default): a write requires a resolved company scope, so an
/// unresolved caller is refused with 403 instead of silently
/// writing a global (unscoped) record. Mutation handlers call
/// this instead of `caller_company`; read handlers keep
/// `caller_company`, and the named carve-outs (self-service
/// writes, settings with no company field, scan/test triggers,
/// platform surfaces) keep it too.
pub(crate) fn require_company_for_write(
    tenant: &unified_core::tenancy::TenantContext,
    req: &actix_web::HttpRequest,
) -> UnifiedResult<Option<String>> {
    match caller_company(tenant, req)? {
        Some(company) => Ok(Some(company)),
        None => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}


/// Verify a request body's `company_id` against the caller's
/// company scope and return the company id to store. Fails closed
/// (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known, a body company is refused: an
/// unresolved caller may not claim a company (the shared
/// `CallerScope::Unresolved` rule); omitting the company is refused (C3 task 315):
/// a write requires a resolved company scope.
pub(crate) fn require_company_match(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
    body_company_id: &Option<String>,
) -> UnifiedResult<Option<String>> {
    let caller = caller_company(tenant, req)?;
    require_company_match_for(&caller, body_company_id)
}

/// `require_company_match` against an already-resolved caller
/// scope, for handlers that computed `caller_company` first.
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
        (None, Some(caller)) => Ok(Some(caller.clone())),
        (None, None) => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}

/// True when a stored record belongs to the caller's company
/// scope. MCP servers carry `company_id: Option<Uuid>`; the scope
/// is the string form. Shared rule
/// (`CallerScope::can_access_record`): a company caller reads its
/// own rows plus global (unscoped) rows; an unresolved caller reads
/// global rows only.
pub(crate) fn in_company_scope(
    record_company_id: &Option<Uuid>,
    caller_company: &Option<String>,
) -> bool {
    match caller_company {
        None => record_company_id.is_none(),
        Some(company) => {
            record_company_id.is_none()
                || record_company_id.as_ref().map(|id| id.to_string()).as_deref()
                    == Some(company.as_str())
        }
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope. Single-record reads and mutations call this
/// right after loading the record; list handlers use
/// `in_company_scope` as a filter instead.
pub(crate) fn require_record_in_company_scope(
    record_company_id: &Option<Uuid>,
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

/// Fail closed (403) when a stored record is outside the caller's
/// company scope FOR A WRITE (C3 task 315, strict by default).
/// Unlike the read check above (rule A), a write requires an exact
/// company match: a company caller may write only their own
/// company's records — global (unscoped) records are platform-only
/// — and an unresolved caller may write nothing. Mutation handlers
/// call this right after loading the record they mutate; handlers
/// that only read the record, or load a parent to scope a child
/// write, keep the read check.
pub(crate) fn require_record_writable_in_company_scope(
    record_company_id: &Option<Uuid>,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    match (record_company_id, caller_company) {
        (Some(record), Some(caller)) => match Uuid::parse_str(caller) {
            Ok(scope) if *record == scope => Ok(()),
            _ => Err(UnifiedError::forbidden(
            "A write requires an exact company match: the record belongs to a different company or is a shared (global) record",
        )),
        },
        _ => Err(UnifiedError::forbidden(
            "A write requires an exact company match: the record belongs to a different company or is a shared (global) record",
        )),
    }
}


/// Verify a `company_id: Option<Uuid>` body field (MCP server
/// create) against the caller's string scope. Same rule as
/// `require_company_match_for`, in the body's `Uuid` shape: an
/// explicit company must equal the caller's; an omitted company
/// adopts the caller scope (parsed as a `Uuid` — an unparseable
/// caller company fails closed rather than storing a row under a
/// scope that can never match); an unresolved caller stores a
/// global row.
pub(crate) fn require_company_match_uuid(
    caller: &Option<String>,
    body_company_id: &Option<Uuid>,
) -> UnifiedResult<Option<Uuid>> {
    match (body_company_id, caller) {
        (Some(body), Some(caller_company)) if body.to_string() != *caller_company => Err(
            UnifiedError::forbidden("company_id does not match the caller's company scope"),
        ),
        (Some(body), Some(_)) => Ok(Some(*body)),
        (Some(_), None) => Err(UnifiedError::forbidden(
            "company_id does not match the caller's company scope",
        )),
        (None, Some(caller_company)) => {
            Uuid::parse_str(caller_company).map(Some).map_err(|_| {
                UnifiedError::forbidden("Caller company id is not a valid company id")
            })
        }
        (None, None) => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}

/// Load an MCP server and verify it is in the caller scope: company
/// callers read their own and global servers only, unresolved
/// callers read global servers only. A missing server and an
/// out-of-scope server both surface as `not_found`, so the check
/// never leaks whether a server exists in another company.
pub(crate) fn require_server_in_scope(
    state: &McpState,
    id_or_slug: &str,
    scope: &Option<String>,
) -> UnifiedResult<McpServer> {
    let server = resolve_server(state, id_or_slug).map_err(|_| {
        UnifiedError::not_found(format!("MCP server \"{id_or_slug}\" not found"))
    })?;
    require_record_in_company_scope(&server.company_id, scope).map_err(|_| {
        UnifiedError::not_found(format!("MCP server \"{id_or_slug}\" not found"))
    })?;
    Ok(server)
}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::test::TestRequest;
    use actix_web::HttpMessage;

    /// Request carrying the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach, plus a verified
    /// caller (optionally with a server-resolved company).
    fn tenant_request(company_id: &str, caller_company: Option<&str>) -> HttpRequest {
        let req = TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: company_id.to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let mut caller =
            unified_core::auth::Caller::verified(test_user_uuid("alice"), Vec::new());
        if let Some(company) = caller_company {
            caller = caller.with_company(company.to_string());
        }
        req.extensions_mut().insert(caller);
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request("acme", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some("acme".to_string())
        );
    }

    #[test]
    fn caller_company_prefers_verified_caller_when_tenant_has_none() {
        let req = tenant_request("", Some("acme"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some("acme".to_string())
        );
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = tenant_request("acme", Some("other"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn caller_company_ignores_the_x_company_id_header() {
        // A forged header must never choose the company: only the
        // tenant context and the verified caller are sources.
        let req = TestRequest::default()
            .insert_header(("X-Company-Id", "forged"))
            .to_http_request();
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
            });
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid("alice"), Vec::new()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(caller_company(&tenant, &req).unwrap(), None);
    }

    #[test]
    fn in_company_scope_follows_the_shared_rule() {
        let company = Uuid::now_v7();
        let scope = Some("acme".to_string());
        // A company caller reads its own rows plus global rows.
        assert!(in_company_scope(&Some(company), &Some(company.to_string())));
        assert!(in_company_scope(&None, &scope));
        assert!(!in_company_scope(&Some(Uuid::now_v7()), &scope));
        // An unresolved caller reads global rows only.
        assert!(in_company_scope(&None, &None));
        assert!(!in_company_scope(&Some(company), &None));
    }

    #[test]
    fn require_company_match_for_refuses_writes_from_unresolved_callers() {
        let body = Some("c1".to_string());
        assert!(require_company_match_for(&None, &body).is_err());
        assert!(require_company_match_for(&None, &None).is_err());
        assert_eq!(
            require_company_match_for(&Some("c1".to_string()), &body).unwrap(),
            Some("c1".to_string())
        );
        assert!(
            require_company_match_for(&Some("c1".to_string()), &Some("c2".to_string())).is_err()
        );
    }

    #[test]
    fn require_company_match_uuid_adopts_the_caller_scope() {
        let company = Uuid::now_v7();
        let scope = Some(company.to_string());
        assert_eq!(
            require_company_match_uuid(&scope, &None).unwrap(),
            Some(company)
        );
        assert_eq!(
            require_company_match_uuid(&scope, &Some(company)).unwrap(),
            Some(company)
        );
        assert!(require_company_match_uuid(&scope, &Some(Uuid::now_v7())).is_err());
        assert!(require_company_match_uuid(&None, &Some(company)).is_err());
        assert_eq!(require_company_match_uuid(&None, &None).unwrap(), None);
        // An unparseable caller company fails closed instead of
        // storing a row under a scope that can never match.
        assert!(require_company_match_uuid(&Some("not-a-uuid".to_string()), &None).is_err());
    }

    #[test]
    fn require_server_in_scope_hides_out_of_scope_servers() {
        let state = McpState::default();
        let company = Uuid::now_v7();
        let server = McpServer {
            id: Uuid::now_v7(),
            slug: "owned".to_string(),
            name: "Owned".to_string(),
            owner_id: Uuid::now_v7(),
            company_id: Some(company),
            site_id: None,
            product_id: None,
            status: "active".to_string(),
            is_public: false,
            allowed_tools: Vec::new(),
            rate_limit_per_minute: 60,
            read_only_role: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        state.servers_by_slug.insert(server.slug.clone(), server.id);
        state.servers.insert(server.id, server.clone());
        assert!(require_server_in_scope(&state, "owned", &Some(company.to_string())).is_ok());
        assert!(require_server_in_scope(&state, "owned", &Some("other".to_string())).is_err());
        assert!(require_server_in_scope(&state, "owned", &None).is_err());
        assert!(require_server_in_scope(&state, "missing", &None).is_err());
    }
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;

    #[test]
    fn column_is_denied_matches_exact_and_compound_names() {
        assert!(column_is_denied("password"));
        assert!(column_is_denied("user_password"));
        assert!(column_is_denied("password_hash"));
        assert!(column_is_denied("my_password_field"));
        assert!(column_is_denied("api_key"));
        assert!(!column_is_denied("username"));
    }

    #[test]
    fn column_is_denied_matches_separator_less_secret_names() {
        // Substring match, not word boundaries: a secret column
        // without separators is denied too.
        assert!(column_is_denied("passwordhash"));
        assert!(column_is_denied("secretkey"));
        assert!(column_is_denied("authtoken"));
        assert!(column_is_denied("tokenized"));
    }

    #[test]
    fn column_is_denied_matches_any_key_name() {
        // The bare "key" fragment denies every key column, not just
        // the listed compounds.
        assert!(column_is_denied("encryption_key"));
        assert!(column_is_denied("signing_key"));
        assert!(column_is_denied("webhook_key"));
        assert!(column_is_denied("key"));
        assert!(column_is_denied("monkey"));
    }

    #[test]
    fn column_is_denied_is_case_insensitive() {
        assert!(column_is_denied("PASSWORD"));
        assert!(column_is_denied("Api_Key"));
    }

    #[test]
    fn find_table_resolves_allowlisted_tables() {
        assert!(find_table("companies").is_some());
        assert!(find_table("store_products").is_some());
        assert!(find_table("nope").is_none());
    }

    #[test]
    fn find_table_is_case_sensitive_and_excludes_secret_tables() {
        assert!(find_table("Companies").is_none());
        assert!(find_table("users").is_none());
    }

    #[test]
    fn find_source_resolves_context_sources() {
        assert!(find_source("site").is_some());
        assert!(find_source("knowledgebase").is_some());
        assert!(find_source("nope").is_none());
    }

    #[test]
    fn tool_is_known_matches_builtin_tools() {
        assert!(tool_is_known("db.query"));
        assert!(tool_is_known("site.search"));
        assert!(!tool_is_known("nope"));
    }

    #[test]
    fn validate_slug_accepts_and_normalizes() {
        assert_eq!(validate_slug("My-Server").unwrap(), "my-server");
        assert_eq!(validate_slug("ok-1").unwrap(), "ok-1");
    }

    #[test]
    fn validate_slug_rejects_short_bad_and_long_names() {
        assert!(validate_slug("ab").is_err());
        assert!(validate_slug("-bad").is_err());
        assert!(validate_slug("bad-").is_err());
        assert!(validate_slug(&"a".repeat(65)).is_err());
    }

    #[test]
    fn validate_role_name_accepts_plain_names() {
        assert_eq!(validate_role_name("reader").unwrap(), "reader");
    }

    #[test]
    fn validate_role_name_rejects_bad_names() {
        assert!(validate_role_name("1abc").is_err());
        assert!(validate_role_name("").is_err());
        assert!(validate_role_name(&"a".repeat(64)).is_err());
    }

    #[test]
    fn validate_tool_list_dedupes_and_validates() {
        let tools = validate_tool_list(&["db.query".to_string(), "db.query".to_string()])
            .expect("known tools pass");
        assert_eq!(tools.len(), 1);
        assert!(validate_tool_list(&["nope".to_string()]).is_err());
        assert!(validate_tool_list(&["".to_string()]).is_err());
    }

    #[test]
    fn validate_rate_limit_bounds() {
        assert!(validate_rate_limit(0).is_err());
        assert_eq!(validate_rate_limit(1).unwrap(), 1);
        assert_eq!(validate_rate_limit(100_000).unwrap(), 100_000);
        assert!(validate_rate_limit(100_001).is_err());
    }

    #[test]
    fn scrub_value_removes_denied_keys_recursively() {
        let value = json!({
            "password": "x",
            "name": "a",
            "nested": {"token": "y", "ok": 1},
        });
        let scrubbed = scrub_value(value);
        assert!(scrubbed.get("password").is_none());
        assert_eq!(scrubbed["name"], json!("a"));
        assert!(scrubbed["nested"].get("token").is_none());
        assert_eq!(scrubbed["nested"]["ok"], json!(1));
    }

    #[test]
    fn scrub_value_keeps_scalars_and_scalar_arrays() {
        assert_eq!(scrub_value(json!(42)), json!(42));
        assert_eq!(scrub_value(json!([1, "a"])), json!([1, "a"]));
    }

    #[test]
    fn scrub_value_removes_separator_less_secret_keys() {
        let scrubbed = scrub_value(json!({
            "passwordhash": "x",
            "encryption_key": "y",
            "name": "a",
        }));
        assert!(scrubbed.get("passwordhash").is_none());
        assert!(scrubbed.get("encryption_key").is_none());
        assert_eq!(scrubbed["name"], json!("a"));
    }

    #[test]
    fn default_allowed_tools_lists_every_builtin_tool() {
        let tools = default_allowed_tools();
        assert_eq!(tools.len(), builtin_tools().len());
        assert!(tools.contains(&"db.query".to_string()));
        assert!(tools.contains(&"site.search".to_string()));
        assert!(tools.iter().all(|tool| tool_is_known(tool)));
    }

    #[test]
    fn presented_key_accepts_any_bearer_scheme_casing() {
        use actix_web::test::TestRequest;
        for scheme in ["Bearer", "bearer", "BEARER", "BeArEr"] {
            let req = TestRequest::default()
                .insert_header(("Authorization", format!("{scheme} abc123")))
                .to_http_request();
            assert_eq!(presented_key(&req).as_deref(), Some("abc123"));
        }
    }

    #[test]
    fn presented_key_rejects_a_bearer_scheme_without_a_key() {
        use actix_web::test::TestRequest;
        let req = TestRequest::default()
            .insert_header(("Authorization", "Bearer "))
            .to_http_request();
        assert!(presented_key(&req).is_none());
        let req = TestRequest::default()
            .insert_header(("Authorization", "Bearer"))
            .to_http_request();
        assert!(presented_key(&req).is_none());
    }

    #[test]
    fn presented_key_falls_back_to_x_api_key() {
        use actix_web::test::TestRequest;
        let req = TestRequest::default()
            .insert_header(("X-Api-Key", "  key-1  "))
            .to_http_request();
        assert_eq!(presented_key(&req).as_deref(), Some("key-1"));
        let req = TestRequest::default()
            .insert_header(("Authorization", "Basic abc"))
            .insert_header(("X-Api-Key", "key-2"))
            .to_http_request();
        assert_eq!(presented_key(&req).as_deref(), Some("key-2"));
    }

    #[test]
    fn caller_accepts_a_verified_caller() {
        use actix_web::test::TestRequest;
        let req = verified_request("user-1", "admin");
        let found = caller(&req).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
    }

    #[test]
    fn caller_rejects_requests_without_a_verified_caller() {
        use actix_web::test::TestRequest;
        let req = TestRequest::default().to_http_request();
        assert!(caller(&req).is_err());
        let req = TestRequest::default().to_http_request();
        assert!(caller(&req).is_err());
    }

    fn test_context() -> AppContext {
        use unified_core::config::AppConfig;
        use unified_core::hooks::HookBus;
        use unified_core::modules::ModuleRegistry;
        use std::sync::Arc;
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    #[test]
    fn require_permission_fails_closed_without_a_role_check_handler() {
        // No `role.check` handler is registered on this bus, so the
        // preset `"allowed": false` must stand and the call is refused.
        let ctx = test_context();
        assert!(!check_capability(&ctx, "admin", PERM_MCP_MANAGE));
        assert!(require_permission(&ctx, "admin", PERM_MCP_MANAGE).is_err());
        assert!(require_permission(&ctx, "admin", PERM_MCP_READ).is_err());
    }

    #[test]
    fn require_permission_accepts_when_role_check_allows() {
        let ctx = test_context();
        ctx.hooks.register(
            "role.check",
            "test",
            std::sync::Arc::new(|context: &mut unified_core::hooks::HookContext| {
                context.set("allowed", serde_json::Value::Bool(true));
            }),
        );
        assert!(require_permission(&ctx, "admin", PERM_MCP_MANAGE).is_ok());
        assert!(require_permission(&ctx, "reader", PERM_MCP_READ).is_ok());
    }
}
