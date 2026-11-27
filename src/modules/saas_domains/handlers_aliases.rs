//! SaaS Domains - alias handlers.
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
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use super::caller::authorize_company;
use super::caller::authorize_list;
use super::caller::caller_from;
use super::caller::conflict_error;
use super::caller::domain_claimed;
use super::caller::require_permission;
use super::events::CAP_MANAGE;
use super::events::EVENT_ALIAS_CREATED;
use super::events::EVENT_ALIAS_RELEASED;
use super::events::EVENT_ALIAS_RESUMED;
use super::events::EVENT_ALIAS_SUSPENDED;
use super::events::EVENT_ALIAS_UPDATED;
use super::requests::CreateAliasRequest;
use super::requests::UpdateAliasRequest;
use super::store::aliases_store;
use super::store::paginate;
use super::store::settings;
use super::types::DomainAlias;
use super::validation::normalize_domain;
use super::validation::validate_target;

// Alias handlers.

pub(crate) async fn create_alias(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateAliasRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DomainAlias> = (|| {
        let caller = caller_from(&req)?;
        authorize_company(&ctx, &caller, body.company_id)?;
        let domain = normalize_domain(&body.domain)?;
        if !company_exists(&ctx, body.company_id) {
            return Err(UnifiedError::not_found("Company not found"));
        }
        let cfg = settings();
        if !cfg.alias_target_types.contains(&body.target_type) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown alias target type '{}' (allowed: {})",
                body.target_type,
                cfg.alias_target_types.join(", ")
            )));
        }
        let target_value = validate_target(&body.target_type, &body.target_value)?;
        if domain_claimed(&ctx, &domain, None, None, None) {
            return Err(conflict_error(&domain));
        }
        if let Some(site_id) = body.site_id {
            if !crate::modules::company::site_exists(&ctx, site_id) {
                return Err(UnifiedError::not_found("Company site not found"));
            }
        }
        let now = Utc::now();
        let alias = DomainAlias {
            id: Uuid::now_v7(),
            domain: domain.clone(),
            company_id: body.company_id,
            target_type: body.target_type.clone(),
            target_value,
            status: "pending_verification".to_string(),
            site_id: body.site_id,
            verified_at: None,
            created_at: now,
            updated_at: now,
        };
        aliases_store().insert(alias.id, alias.clone());
        ctx.hooks.emit(
            EVENT_ALIAS_CREATED,
            json!({
                "alias_id": alias.id,
                "domain": alias.domain,
                "company_id": alias.company_id,
                "target_type": alias.target_type,
                "target_value": alias.target_value,
            }),
        );
        Ok(alias)
    })();
    match result {
        Ok(row) => json::success_response(row, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_aliases(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_from(&req)?;
        let scope = authorize_list(&ctx, &caller)?;
        let mut items: Vec<DomainAlias> = aliases_store()
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
                        "Cannot list another company's aliases",
                    ));
                }
                items.retain(|row| row.company_id == wanted);
            }
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|row| row.status == *status);
        }
        if let Some(target_type) = params.filters.get("target_type") {
            items.retain(|row| row.target_type == *target_type);
        }
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| {
                row.domain.to_lowercase().contains(&needle)
                    || row.target_value.to_lowercase().contains(&needle)
            });
        }
        let sort = params.allowed_sort(
            &["domain", "status", "target_type", "created_at"],
            "created_at",
        );
        match sort.as_str() {
            "domain" => items.sort_by(|a, b| a.domain.cmp(&b.domain)),
            "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
            "target_type" => items.sort_by(|a, b| a.target_type.cmp(&b.target_type)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_alias(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DomainAlias> = (|| {
        let caller = caller_from(&req)?;
        let row = aliases_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Alias not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        Ok(row)
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_alias(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateAliasRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DomainAlias> = (|| {
        let caller = caller_from(&req)?;
        let id = id.into_inner();
        let mut row = aliases_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Alias not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        if row.status == "released" {
            return Err(UnifiedError::conflict("A released alias cannot be updated"));
        }
        let cfg = settings();
        let target_type = body
            .target_type
            .clone()
            .unwrap_or_else(|| row.target_type.clone());
        if !cfg.alias_target_types.contains(&target_type) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown alias target type '{target_type}' (allowed: {})",
                cfg.alias_target_types.join(", ")
            )));
        }
        let target_value = match &body.target_value {
            Some(value) => validate_target(&target_type, value)?,
            None => {
                if body.target_type.is_some() {
                    validate_target(&target_type, &row.target_value)?
                } else {
                    row.target_value.clone()
                }
            }
        };
        if let Some(site_id) = body.site_id {
            match site_id {
                Some(site) => {
                    if !crate::modules::company::site_exists(&ctx, site) {
                        return Err(UnifiedError::not_found("Company site not found"));
                    }
                    row.site_id = Some(site);
                }
                None => row.site_id = None,
            }
        }
        row.target_type = target_type;
        row.target_value = target_value;
        row.updated_at = Utc::now();
        aliases_store().insert(row.id, row.clone());
        ctx.hooks.emit(
            EVENT_ALIAS_UPDATED,
            json!({
                "alias_id": row.id,
                "domain": row.domain,
                "company_id": row.company_id,
                "target_type": row.target_type,
                "target_value": row.target_value,
            }),
        );
        Ok(row)
    })();
    json::respond(result, debug)
}

pub(crate) async fn suspend_alias(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DomainAlias> = (|| {
        let caller = caller_from(&req)?;
        let id = id.into_inner();
        let mut row = aliases_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Alias not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        if row.status != "verified" && row.status != "pending_verification" {
            return Err(UnifiedError::conflict(format!(
                "Only a verified or pending alias can be suspended (current status: {})",
                row.status
            )));
        }
        row.status = "suspended".to_string();
        row.updated_at = Utc::now();
        aliases_store().insert(row.id, row.clone());
        ctx.hooks.emit(
            EVENT_ALIAS_SUSPENDED,
            json!({
                "alias_id": row.id,
                "domain": row.domain,
                "company_id": row.company_id,
            }),
        );
        Ok(row)
    })();
    json::respond(result, debug)
}

/// Resume a suspended alias. Suspension is not terminal: only an
/// admin may resume (the tenant cannot undo its own suspension or
/// an admin's). The alias returns to the status its verification
/// state implies - `verified` when verification completed before
/// suspension, `pending_verification` otherwise - mirroring the
/// suspend event with a dedicated resumed event.
pub(crate) async fn resume_alias(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DomainAlias> = (|| {
        let caller = caller_from(&req)?;
        require_permission(&ctx, &caller.role, CAP_MANAGE)?;
        let id = id.into_inner();
        let mut row = aliases_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Alias not found"))?;
        if row.status != "suspended" {
            return Err(UnifiedError::conflict(format!(
                "Only a suspended alias can be resumed (current status: {})",
                row.status
            )));
        }
        row.status = if row.verified_at.is_some() {
            "verified".to_string()
        } else {
            "pending_verification".to_string()
        };
        row.updated_at = Utc::now();
        aliases_store().insert(row.id, row.clone());
        ctx.hooks.emit(
            EVENT_ALIAS_RESUMED,
            json!({
                "alias_id": row.id,
                "domain": row.domain,
                "company_id": row.company_id,
                "status": row.status,
            }),
        );
        Ok(row)
    })();
    json::respond(result, debug)
}

pub(crate) async fn release_alias(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DomainAlias> = (|| {
        let caller = caller_from(&req)?;
        let id = id.into_inner();
        let mut row = aliases_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Alias not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        if row.status == "released" {
            return Err(UnifiedError::conflict("Alias is already released"));
        }
        row.status = "released".to_string();
        row.updated_at = Utc::now();
        aliases_store().insert(row.id, row.clone());
        ctx.hooks.emit(
            EVENT_ALIAS_RELEASED,
            json!({
                "alias_id": row.id,
                "domain": row.domain,
                "company_id": row.company_id,
            }),
        );
        Ok(row)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_alias(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_from(&req)?;
        let id = id.into_inner();
        let row = aliases_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Alias not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        aliases_store().remove(&id);
        ctx.hooks.emit(
            EVENT_ALIAS_RELEASED,
            json!({
                "alias_id": row.id,
                "domain": row.domain,
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
    use chrono::DateTime;

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

    fn seed_alias(status: &str, verified_at: Option<DateTime<Utc>>) -> DomainAlias {
        let now = Utc::now();
        let row = DomainAlias {
            id: Uuid::now_v7(),
            domain: format!("alias-{}.example.com", Uuid::now_v7().as_simple()),
            company_id: Uuid::now_v7(),
            target_type: "cname_subdomain".to_string(),
            target_value: "app.example.com".to_string(),
            status: status.to_string(),
            site_id: None,
            verified_at,
            created_at: now,
            updated_at: now,
        };
        aliases_store().insert(row.id, row.clone());
        row
    }

    #[tokio::test]
    async fn resume_verified_alias_returns_to_verified() {
        let row = seed_alias("suspended", Some(Utc::now()));
        let resp = resume_alias(
            test_ctx(&[CAP_MANAGE]),
            test_request(),
            web::Path::from(row.id),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            aliases_store().get(&row.id).unwrap().value().status,
            "verified"
        );
        aliases_store().remove(&row.id);
    }

    #[tokio::test]
    async fn resume_unverified_alias_returns_to_pending_verification() {
        let row = seed_alias("suspended", None);
        let resp = resume_alias(
            test_ctx(&[CAP_MANAGE]),
            test_request(),
            web::Path::from(row.id),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            aliases_store().get(&row.id).unwrap().value().status,
            "pending_verification"
        );
        aliases_store().remove(&row.id);
    }

    #[tokio::test]
    async fn resume_active_alias_is_conflict() {
        let row = seed_alias("verified", Some(Utc::now()));
        let resp = resume_alias(
            test_ctx(&[CAP_MANAGE]),
            test_request(),
            web::Path::from(row.id),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(
            aliases_store().get(&row.id).unwrap().value().status,
            "verified"
        );
        aliases_store().remove(&row.id);
    }

    #[tokio::test]
    async fn resume_alias_requires_admin() {
        let row = seed_alias("suspended", Some(Utc::now()));
        let resp = resume_alias(test_ctx(&[]), test_request(), web::Path::from(row.id)).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            aliases_store().get(&row.id).unwrap().value().status,
            "suspended"
        );
        aliases_store().remove(&row.id);
    }
}
