//! SaaS Domains - subdomain allocation handlers.
//!
//! Part of the `saas_domains` module split; see `src/modules/saas_domains.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use unified_core::json;
use unified_core::pagination::PaginationParams;
use crate::modules::company::company_exists;
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use actix_web::web;
use chrono::Utc;
use diesel::prelude::*;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use super::caller::authorize_company;
use super::caller::authorize_list;
use super::caller::caller_from;
use super::caller::check_capability;
use super::caller::conflict_error;
use super::caller::domain_claimed;
use super::caller::require_permission;
use super::events::CAP_MANAGE;
use super::events::CAP_SELF_ASSIGN;
use super::events::EVENT_SUBDOMAIN_APPROVED;
use super::events::EVENT_SUBDOMAIN_CREATED;
use super::events::EVENT_SUBDOMAIN_REJECTED;
use super::events::EVENT_SUBDOMAIN_RELEASED;
use super::events::EVENT_SUBDOMAIN_RESUMED;
use super::events::EVENT_SUBDOMAIN_SUSPENDED;
use super::requests::CreateSubdomainRequest;
use super::store::allocations_store;
use super::store::paginate;
use super::store::settings;
use super::types::SubdomainAllocation;
use super::validation::validate_label;

// ---------------------------------------------------------------------------
// Subdomain allocation handlers.
// ---------------------------------------------------------------------------

/// Count live subdomain allocations for a tenant. Live means
/// `active`, `pending_approval`, or `suspended`: a suspended
/// allocation still holds its hostname (and its quota slot), so
/// suspending must not free quota for new allocations. Released
/// and rejected allocations never count. Shared by the HTTP create
/// path and the event claim path so the quota means the same thing
/// in both.
pub(crate) fn count_live_allocations(company_id: Uuid) -> usize {
    allocations_store()
        .iter()
        .filter(|entry| {
            let row = entry.value();
            row.company_id == company_id
                && (row.status == "active"
                    || row.status == "pending_approval"
                    || row.status == "suspended")
        })
        .count()
}

pub(crate) async fn create_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateSubdomainRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SubdomainAllocation> = (|| {
        let caller = caller_from(&req)?;
        let cfg = settings();
        if cfg.wildcard_base.trim().is_empty() {
            return Err(UnifiedError::unprocessable(
                "Tenant subdomains are not configured: set saas_domains.wildcard_base first",
            ));
        }
        let is_admin = check_capability(&ctx, &caller.role, CAP_MANAGE);
        if is_admin {
            // Admin assignment: any tenant.
        } else {
            if !check_capability(&ctx, &caller.role, CAP_SELF_ASSIGN)
                || caller.company_id != Some(body.company_id)
            {
                return Err(UnifiedError::forbidden(
                    "Caller may only self-assign subdomains for its own company",
                ));
            }
        }
        if !company_exists(&ctx, body.company_id) {
            return Err(UnifiedError::not_found("Company not found"));
        }
        let label = validate_label(&body.label, &cfg)?;
        let hostname = format!("{label}.{}", cfg.wildcard_base.trim().to_lowercase());
        if domain_claimed(&ctx, &hostname, None, None, None) {
            return Err(conflict_error(&hostname));
        }
        let live_count = count_live_allocations(body.company_id);
        if u32::try_from(live_count).unwrap_or(u32::MAX) >= cfg.max_subdomains_per_tenant {
            return Err(UnifiedError::conflict(format!(
                "Tenant already holds the maximum of {} subdomain allocations",
                cfg.max_subdomains_per_tenant
            )));
        }
        if let Some(site_id) = body.site_id {
            if !crate::modules::company::site_exists(&ctx, site_id) {
                return Err(UnifiedError::not_found("Company site not found"));
            }
        }
        let now = Utc::now();
        // Admin assignments are active immediately; tenant
        // self-assignments stay pending when approval is required
        // (config key saas_domains.require_subdomain_approval).
        let status = if is_admin || !cfg.require_subdomain_approval {
            "active".to_string()
        } else {
            "pending_approval".to_string()
        };
        let allocation = SubdomainAllocation {
            id: Uuid::now_v7(),
            label,
            hostname: hostname.clone(),
            company_id: body.company_id,
            site_id: body.site_id,
            status,
            approved_by: if is_admin {
                Some(caller.actor.clone())
            } else {
                None
            },
            created_at: now,
            updated_at: now,
        };
        allocations_store().insert(allocation.id, allocation.clone());
        ctx.hooks.emit(
            EVENT_SUBDOMAIN_CREATED,
            json!({
                "allocation_id": allocation.id,
                "label": allocation.label,
                "hostname": allocation.hostname,
                "company_id": allocation.company_id,
                "status": allocation.status,
            }),
        );
        Ok(allocation)
    })();
    match result {
        Ok(row) => json::success_response(row, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_subdomains(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_from(&req)?;
        let scope = authorize_list(&ctx, &caller)?;
        let mut items: Vec<SubdomainAllocation> = allocations_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(company_id) = scope {
            items.retain(|row| row.company_id == company_id);
        }
        if let Some(company_id) = params.filters.get("company_id") {
            if let Ok(wanted) = company_id.parse::<Uuid>() {
                if scope.is_some_and(|scoped| scoped != wanted) {
                    return Err(UnifiedError::forbidden(
                        "Cannot list another company's subdomains",
                    ));
                }
                items.retain(|row| row.company_id == wanted);
            }
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|row| row.status == *status);
        }
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| row.hostname.to_lowercase().contains(&needle));
        }
        let sort =
            params.allowed_sort(&["label", "hostname", "status", "created_at"], "created_at");
        match sort.as_str() {
            "label" => items.sort_by(|a, b| a.label.cmp(&b.label)),
            "hostname" => items.sort_by(|a, b| a.hostname.cmp(&b.hostname)),
            "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SubdomainAllocation> = (|| {
        let caller = caller_from(&req)?;
        let row = allocations_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Subdomain allocation not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        Ok(row)
    })();
    json::respond(result, debug)
}

fn transition_allocation(
    ctx: &AppContext,
    req: &HttpRequest,
    id: Uuid,
    from: &[&str],
    to: &str,
    event: &str,
    admin_only: bool,
) -> UnifiedResult<SubdomainAllocation> {
    let caller = caller_from(req)?;
    let mut row = allocations_store()
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Subdomain allocation not found"))?;
    if admin_only {
        require_permission(ctx, &caller.role, CAP_MANAGE)?;
    } else {
        authorize_company(ctx, &caller, row.company_id)?;
    }
    if !from.contains(&row.status.as_str()) {
        return Err(UnifiedError::conflict(format!(
            "Allocation in status '{}' cannot move to '{to}' (allowed from: {})",
            row.status,
            from.join(", ")
        )));
    }
    row.status = to.to_string();
    if to == "active" {
        row.approved_by = Some(caller.actor.clone());
    }
    row.updated_at = Utc::now();
    allocations_store().insert(row.id, row.clone());
    ctx.hooks.emit(
        event,
        json!({
            "allocation_id": row.id,
            "label": row.label,
            "hostname": row.hostname,
            "company_id": row.company_id,
            "status": row.status,
        }),
    );
    Ok(row)
}

pub(crate) async fn approve_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = transition_allocation(
        &ctx,
        &req,
        id.into_inner(),
        &["pending_approval"],
        "active",
        EVENT_SUBDOMAIN_APPROVED,
        true,
    );
    json::respond(result, debug)
}

pub(crate) async fn reject_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = transition_allocation(
        &ctx,
        &req,
        id.into_inner(),
        &["pending_approval"],
        "rejected",
        EVENT_SUBDOMAIN_REJECTED,
        true,
    );
    json::respond(result, debug)
}

pub(crate) async fn suspend_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = transition_allocation(
        &ctx,
        &req,
        id.into_inner(),
        &["active"],
        "suspended",
        EVENT_SUBDOMAIN_SUSPENDED,
        false,
    );
    json::respond(result, debug)
}

/// Resume a suspended allocation back to `active`. Suspension is
/// not terminal: only an admin may resume (the tenant cannot undo
/// its own suspension or an admin's), and the resume is recorded
/// with the admin as approver, mirroring the suspend event.
pub(crate) async fn resume_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = transition_allocation(
        &ctx,
        &req,
        id.into_inner(),
        &["suspended"],
        "active",
        EVENT_SUBDOMAIN_RESUMED,
        true,
    );
    json::respond(result, debug)
}

pub(crate) async fn release_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = transition_allocation(
        &ctx,
        &req,
        id.into_inner(),
        &["active", "suspended", "pending_approval"],
        "released",
        EVENT_SUBDOMAIN_RELEASED,
        false,
    );
    json::respond(result, debug)
}

pub(crate) async fn delete_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_from(&req)?;
        let id = id.into_inner();
        let row = allocations_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Subdomain allocation not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        allocations_store().remove(&id);
        ctx.hooks.emit(
            EVENT_SUBDOMAIN_RELEASED,
            json!({
                "allocation_id": row.id,
                "hostname": row.hostname,
                "company_id": row.company_id,
                "deleted": true,
            }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
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
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::hooks::HookContext;
    use unified_core::modules::ModuleRegistry;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    /// App context whose `role.check` hook allows exactly the
    /// given permissions (all other permissions are denied).
    fn test_ctx(allowed: &[&str]) -> web::Data<Arc<AppContext>> {
        let hooks = Arc::new(HookBus::new());
        let allowed: Vec<String> = allowed
            .iter()
            .map(|permission| permission.to_string())
            .collect();
        hooks.register(
            "role.check",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                let permission = ctx
                    .get("permission")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                ctx.set(
                    "allowed",
                    serde_json::json!(allowed.iter().any(|held| held == permission)),
                );
            }),
        );
        let ctx = AppContext::new(
            AppConfig::default(),
            hooks,
            Arc::new(ModuleRegistry::new()),
        );
        web::Data::new(Arc::new(ctx))
    }

    fn test_request() -> HttpRequest {
        verified_request("tester", "admin")
    }

    fn seed_allocation(company_id: Uuid, status: &str) -> SubdomainAllocation {
        let now = Utc::now();
        let label = format!("test-{}", Uuid::now_v7().as_simple());
        let row = SubdomainAllocation {
            id: Uuid::now_v7(),
            label: label.clone(),
            hostname: format!("{label}.tenants.example.com"),
            company_id,
            site_id: None,
            status: status.to_string(),
            approved_by: None,
            created_at: now,
            updated_at: now,
        };
        allocations_store().insert(row.id, row.clone());
        row
    }

    #[test]
    fn count_live_allocations_includes_suspended() {
        let company = Uuid::now_v7();
        let mut ids = Vec::new();
        for status in ["active", "pending_approval", "suspended", "released", "rejected"] {
            ids.push(seed_allocation(company, status).id);
        }
        // Only active, pending_approval, and suspended are live.
        assert_eq!(count_live_allocations(company), 3);
        for id in ids {
            allocations_store().remove(&id);
        }
    }

    #[tokio::test]
    async fn resume_suspended_allocation_returns_to_active() {
        let row = seed_allocation(Uuid::now_v7(), "suspended");
        let resp = resume_subdomain(
            test_ctx(&[CAP_MANAGE]),
            test_request(),
            web::Path::from(row.id),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let updated = allocations_store().get(&row.id).unwrap();
        assert_eq!(updated.value().status, "active");
        assert_eq!(
            updated.value().approved_by.as_deref(),
            Some(test_user_uuid("tester").to_string().as_str())
        );
        allocations_store().remove(&row.id);
    }

    #[tokio::test]
    async fn resume_active_allocation_is_conflict() {
        let row = seed_allocation(Uuid::now_v7(), "active");
        let resp = resume_subdomain(
            test_ctx(&[CAP_MANAGE]),
            test_request(),
            web::Path::from(row.id),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(
            allocations_store().get(&row.id).unwrap().value().status,
            "active"
        );
        allocations_store().remove(&row.id);
    }

    #[tokio::test]
    async fn resume_allocation_requires_admin() {
        let row = seed_allocation(Uuid::now_v7(), "suspended");
        let resp = resume_subdomain(test_ctx(&[]), test_request(), web::Path::from(row.id)).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            allocations_store().get(&row.id).unwrap().value().status,
            "suspended"
        );
        allocations_store().remove(&row.id);
    }
}
