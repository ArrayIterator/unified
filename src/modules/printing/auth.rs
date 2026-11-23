//! printing - auth.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::tenancy::TenantContext as TenancyContext;
use actix_web::{HttpRequest};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Serialize};
use serde_json::{json};
use super::{events::*, paper::*, thermal::*, profiles::*, jobs::*, state::*, hooks::*, handlers_paper::*, handlers_profiles::*, handlers_jobs::*, module::*};



// ---------------------------------------------------------------------------
// Caller identity and capability checks
// ---------------------------------------------------------------------------
/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the assets and POS modules.
/// `tenant_id` is the resolved tenant (uuidv7) from the
/// middleware-attached context — every handler scopes its reads
/// and writes to it (see `tenant_visible`).
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
    pub(crate) tenant_id: String,
}


/// Resolve the tenant context for a request. THE single
/// convergence point for tenant-scoped data access in this file
/// (see `unified_core::tenancy`): this function returns the tenancy
/// `TenantContext` (tenant id as Uuid + schema name derived only
/// by `tenancy::schema_name()`), and the Diesel persistence wiring
/// — when it lands — takes the Uuid from here into
/// `tenancy::with_tenant_connection`. Fail closed:
/// `from_http_request` answers 401 when the request did not
/// resolve to a tenant and 403 when the resolved tenant id is not
/// a valid UUID; no resolved tenant means the request is refused
/// here, never served from `public`.
///
/// Tenant isolation (closed): every record carries `tenant_id`
/// (`PrintJob`, `PaperSize`, `PrinterProfile`) and every list/get
/// filters or checks through `tenant_visible` /
/// `require_same_tenant`, so one tenant's caller cannot read or
/// act on another tenant's jobs, paper sizes, or profiles.
/// Records with no tenant are the exception: the global standard
/// paper sizes (visible to every tenant by design) and records
/// created through a hook that carried no tenant context — the
/// billing module does not pass `tenant_id` in
/// `printing.job.requested` yet, so hook-created jobs are
/// tenant-less until it does. When the Diesel persistence wiring
/// lands, the per-schema store replaces this in-memory filtering.
pub(crate) fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<TenancyContext> {
    TenancyContext::from_http_request(req)
}


/// True when a record is visible to the caller's tenant: records
/// with no tenant (global standards, pre-stamping or hook-created
/// records) are visible to every tenant; a record with a tenant
/// is visible only to that tenant.
pub(crate) fn tenant_visible(record_tenant_id: Option<&str>, tenant_id: &str) -> bool {
    record_tenant_id.is_none_or(|owner| owner == tenant_id)
}


/// Single-record ownership check: a record owned by another
/// tenant answers 404 (not found) and never leaks its existence.
/// Tenant-less records pass: they predate tenant stamping (or
/// arrived through a hook with no tenant context) and the
/// dispatcher must still be able to process them.
pub(crate) fn require_same_tenant(
    record_tenant_id: Option<&str>,
    tenant_id: &str,
) -> UnifiedResult<()> {
    if tenant_visible(record_tenant_id, tenant_id) {
        Ok(())
    } else {
        Err(UnifiedError::not_found("Record not found"))
    }
}


pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    // Tenant first: no resolved tenant context means fail closed,
    // never fall back to the public schema. The resolved tenant
    // id is kept on the caller so every handler can scope its
    // reads and writes to it (see `tenant_visible`).
    let tenant = resolve_tenant(req)?;
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller {
        actor,
        role,
        tenant_id: tenant.tenant_id.to_string(),
    })
}


/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the assets module's capability check).
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


pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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


/// Notify one user through `notification.requested`. The
/// notifications module resolves `user_ids`; when it is not
/// registered the event simply has no listener, which is fine.
pub(crate) fn notify_user(state: &PrintingState, user_id: &str, title: &str, body: &str, job_id: &str) {
    state.emit(
        "notification.requested",
        json!({
            "kind": "invoice",
            "title": title,
            "body": body,
            "user_ids": [user_id],
            "module": "printing",
            "priority": "normal",
            "link": format!("/api/v1/printing/print-jobs/{job_id}"),
        }),
    );
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_visibility_isolates_tenant_owned_records() {
        // A record owned by tenant A is visible to A only.
        assert!(tenant_visible(Some("tenant-a"), "tenant-a"));
        assert!(!tenant_visible(Some("tenant-a"), "tenant-b"));
        assert!(require_same_tenant(Some("tenant-a"), "tenant-a").is_ok());
        assert!(require_same_tenant(Some("tenant-a"), "tenant-b").is_err());
        // Tenant-less records (global standards, hook-created jobs)
        // stay visible to every tenant.
        assert!(tenant_visible(None, "tenant-a"));
        assert!(tenant_visible(None, "tenant-b"));
        assert!(require_same_tenant(None, "tenant-a").is_ok());
    }
}
