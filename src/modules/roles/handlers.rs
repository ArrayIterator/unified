//! roles - handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events};
use unified_core::json::{error_response, success_response};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Utc};
use serde_json::{json};
use std::sync::Arc;
use uuid::Uuid;
use super::{store::*, types::*, module::*, capabilities::*, seeds::*, validation::*};


// Caller identity and capability checks (same trusted-header pattern
// as the other modules: X-User-Id plus X-Role headers, `role.check`
// hook, fail closed). Role administration is admin-only: without
// this gate anyone could mint a `{"*": ["*"]}` role or rewrite the
// admin role's permissions, which would undermine every other
// module's `role.check`.
/// Caller identity: `X-Role` carries the role slug the capability
/// check runs against; `X-User-Id` must also be present, so an
/// anonymous caller is rejected before any capability is consulted.
struct Caller {
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let role = verified.role().to_string();
    Ok(Caller { role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
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

/// Authorize one request: caller identity + one capability.
fn authorize(ctx: &AppContext, req: &HttpRequest, permission: &str) -> UnifiedResult<Caller> {
    let found = caller(req)?;
    require_permission(ctx, &found.role, permission)?;
    Ok(found)
}


pub(crate) async fn list_roles(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<RolesStore>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_ROLES_VIEW) {
        return error_response(&error, ctx.is_debug());
    }
    let pagination = query.normalized();
    let sort = query.allowed_sort(&["name", "slug", "created_at"], "created_at");
    let descending = pagination.order == "desc";
    let search = pagination.search.clone().map(|s| s.to_lowercase());
    let is_system_filter = pagination
        .filters
        .get("is_system")
        .and_then(|value| value.parse::<bool>().ok());

    let mut records: Vec<RoleRecord> = store
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| {
            if let Some(search) = &search {
                let haystack = format!("{} {}", record.name, record.slug).to_lowercase();
                if !haystack.contains(search.as_str()) {
                    return false;
                }
            }
            if let Some(is_system) = is_system_filter {
                if record.is_system != is_system {
                    return false;
                }
            }
            true
        })
        .collect();
    records.sort_by(|a, b| match sort.as_str() {
        "name" => a.name.cmp(&b.name),
        "slug" => a.slug.cmp(&b.slug),
        _ => a.created_at.cmp(&b.created_at),
    });
    if descending {
        records.reverse();
    }
    let total = records.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(records.len());
    let items = if start < records.len() {
        records[start..end].to_vec()
    } else {
        Vec::new()
    };
    success_response(PageResult::new(items, total, &pagination).to_json(), 200)
}


pub(crate) async fn get_role(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<RolesStore>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_ROLES_VIEW) {
        return error_response(&error, ctx.is_debug());
    }
    let key = id.into_inner();
    match find_role(&store, &key) {
        Some(record) => success_response(record, 200),
        None => error_response(
            &UnifiedError::not_found(format!("Role \"{key}\" not found")),
            false,
        ),
    }
}


pub(crate) async fn create_role(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<RolesStore>,
    req: HttpRequest,
    body: web::Json<CreateRoleInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<RoleRecord> = (|| {
        authorize(&ctx, &req, PERM_ROLES_MANAGE)?;
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Role name must not be empty"));
        }
        // The slug is normalised before validation, like the name:
        // a leading or trailing space must not turn into a 400.
        let slug = input.slug.trim().to_string();
        if !valid_slug(&slug) {
            return Err(UnifiedError::bad_request(
                "Role slug must be lowercase letters, digits, '-' or '_'",
            ));
        }
        validate_permissions(&input.permissions)?;
        if store.iter().any(|entry| entry.value().slug == slug) {
            return Err(UnifiedError::conflict(format!(
                "A role with slug \"{slug}\" already exists"
            )));
        }
        let now = Utc::now();
        let record = RoleRecord {
            id: Uuid::now_v7(),
            name: input.name.trim().to_string(),
            slug,
            description: input.description,
            is_system: false,
            permissions: input.permissions,
            created_at: now,
            updated_at: now,
        };
        store.insert(record.id, record.clone());
        Ok(record)
    })();
    match result {
        Ok(record) => {
            ctx.hooks.emit(
                events::ROLE_CREATED,
                json!({ "id": record.id, "slug": record.slug, "name": record.name }),
            );
            success_response(record, 201)
        }
        Err(error) => error_response(&error, debug),
    }
}


pub(crate) async fn update_role(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<RolesStore>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<UpdateRoleInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let key = id.into_inner();
    let input = body.into_inner();
    let result: UnifiedResult<RoleRecord> = (|| {
        authorize(&ctx, &req, PERM_ROLES_MANAGE)?;
        let mut record = match find_role(&store, &key) {
            Some(record) => record,
            None => {
                return Err(UnifiedError::not_found(format!(
                    "Role \"{key}\" not found"
                )))
            }
        };
        if let Some(name) = &input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Role name must not be empty"));
            }
            record.name = name.trim().to_string();
        }
        if let Some(slug) = &input.slug {
            if record.is_system {
                return Err(UnifiedError::conflict(
                    "System roles cannot be renamed",
                ));
            }
            if !valid_slug(slug) {
                return Err(UnifiedError::bad_request(
                    "Role slug must be lowercase letters, digits, '-' or '_'",
                ));
            }
            if store
                .iter()
                .any(|entry| entry.key() != &record.id && entry.value().slug == *slug)
            {
                return Err(UnifiedError::conflict(format!(
                    "A role with slug \"{slug}\" already exists"
                )));
            }
            record.slug = slug.clone();
        }
        if let Some(description) = input.description {
            record.description = description;
        }
        if let Some(permissions) = &input.permissions {
            validate_permissions(permissions)?;
            record.permissions = permissions.clone();
        }
        record.updated_at = Utc::now();
        store.insert(record.id, record.clone());
        Ok(record)
    })();
    match result {
        Ok(record) => success_response(record, 200),
        Err(error) => error_response(&error, debug),
    }
}


pub(crate) async fn delete_role(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<RolesStore>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_ROLES_MANAGE) {
        return error_response(&error, debug);
    }
    let key = id.into_inner();
    let record = match find_role(&store, &key) {
        Some(record) => record,
        None => {
            return error_response(
                &UnifiedError::not_found(format!("Role \"{key}\" not found")),
                debug,
            )
        }
    };
    if record.is_system {
        return error_response(
            &UnifiedError::conflict(format!(
                "System role \"{}\" cannot be deleted",
                record.slug
            )),
            debug,
        );
    }
    store.remove(&record.id);
    success_response(json!({ "deleted": true, "id": record.id }), 200)
}


pub(crate) async fn get_role_permissions(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<RolesStore>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_ROLES_VIEW) {
        return error_response(&error, debug);
    }
    let key = id.into_inner();
    match find_role(&store, &key) {
        Some(record) => success_response(
            json!({ "id": record.id, "slug": record.slug, "permissions": record.permissions }),
            200,
        ),
        None => error_response(
            &UnifiedError::not_found(format!("Role \"{key}\" not found")),
            debug,
        ),
    }
}


pub(crate) async fn check_permission(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<RolesStore>,
    req: HttpRequest,
    body: web::Json<CheckRoleInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    // The check endpoint is an oracle over every role's permissions,
    // so it is admin-only like the mutations.
    if let Err(error) = authorize(&ctx, &req, PERM_ROLES_MANAGE) {
        return error_response(&error, debug);
    }
    let input = body.into_inner();
    let allowed = match check_role(&store, &input.role, &input.permission) {
        Some(allowed) => allowed,
        None => {
            return error_response(
                &UnifiedError::not_found(format!("Role \"{}\" not found", input.role)),
                debug,
            )
        }
    };
    // The hook lets other modules override the check result (for example
    // a company-scoped role manager). The payload value wins over the
    // locally computed one.
    let hook = ctx.hooks.emit(
        "role.check",
        json!({ "role": input.role, "permission": input.permission, "allowed": allowed }),
    );
    let allowed = hook
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(allowed);
    success_response(
        CheckRoleResponse {
            role: input.role,
            permission: input.permission,
            allowed,
        },
        200,
    )
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
    use unified_core::modules::{Module, ModuleRegistry};
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;
    use dashmap::DashMap;

    /// Context whose `role.check` hook is answered by the real roles
    /// module (seeded admin holds `{"*": ["*"]}`; member holds no
    /// roles capability), so the gate is tested end to end.
    fn gated_context() -> AppContext {
        let bus = Arc::new(HookBus::new());
        RolesModule::new().register_hooks(&bus);
        AppContext::new(AppConfig::default(), bus, Arc::new(ModuleRegistry::new()))
    }

    fn data(ctx: AppContext) -> web::Data<Arc<AppContext>> {
        web::Data::new(Arc::new(ctx))
    }

    fn empty_store() -> web::Data<RolesStore> {
        web::Data::new(Arc::new(DashMap::new()))
    }

    fn authed_request(role: &str) -> HttpRequest {
        verified_request("user-1", role)
    }

    fn create_input(slug: &str) -> web::Json<CreateRoleInput> {
        web::Json(CreateRoleInput {
            name: "Admins".to_string(),
            slug: slug.to_string(),
            description: None,
            permissions: json!({}),
        })
    }

    #[tokio::test]
    async fn list_roles_rejects_anonymous_callers() {
        let response = list_roles(
            data(gated_context()),
            empty_store(),
            TestRequest::default().to_http_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create_role_rejects_role_without_manage() {
        let response = create_role(
            data(gated_context()),
            empty_store(),
            authed_request("member"),
            create_input("admins"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn check_permission_rejects_anonymous_callers() {
        let response = check_permission(
            data(gated_context()),
            empty_store(),
            TestRequest::default().to_http_request(),
            web::Json(CheckRoleInput {
                role: "admin".to_string(),
                permission: "posts:create".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create_role_allows_admin_and_trims_slug() {
        let store = empty_store();
        let response = create_role(
            data(gated_context()),
            store.clone(),
            authed_request("admin"),
            create_input(" admins "),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        assert!(store.iter().any(|entry| entry.value().slug == "admins"));
        assert!(!store.iter().any(|entry| entry.value().slug == " admins "));
    }
}
