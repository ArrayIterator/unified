// Handlers: admin.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Handlers: admin.
// ---------------------------------------------------------------------------

/// Admin identity comes ONLY from request headers (`X-User-Id`
/// actor, `X-Role` role slug) via `admin_caller`. Admin endpoints
/// run on the control plane: they never take a tenant scope from a
/// request, and they never fall back to a tenant context — the
/// caller must hold the capability and name the tenant explicitly
/// in the filter or body. Query parameters (`?viewer_role=` /
/// `?viewer_id=`) and body actor fields are never identity: a
/// role slug named in a URL authorizes nothing. `granted_by` in a
/// request body is kept only as an audit note on the created row,
/// never as the authorizer. Callers missing either identity header
/// get 401; a caller whose role lacks the capability gets 403.
pub(crate) async fn list_entitlements(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = admin_caller(&req)?;
        require_capability(&ctx, caller.role.as_deref(), PERM_ENTITLEMENTS_MANAGE)?;
        let mut items: Vec<Entitlement> = state
            .entitlements
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                matches_search(
                    &format!("{} {} {} {}", row.company_id, row.module_id, row.source_kind, row.source_id),
                    &params,
                )
                    && params
                        .filters
                        .get("company_id")
                        .is_none_or(|company_id| row.company_id == *company_id)
                    && params
                        .filters
                        .get("module_id")
                        .is_none_or(|module_id| row.module_id == *module_id)
                    && params
                        .filters
                        .get("source_kind")
                        .is_none_or(|source_kind| row.source_kind == *source_kind)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| row.status == *status)
            })
            .collect();
        let sort = params.allowed_sort(&["created_at", "module_id", "company_id"], "created_at");
        match sort.as_str() {
            "module_id" => items.sort_by(|a, b| a.module_id.cmp(&b.module_id)),
            "company_id" => items.sort_by(|a, b| a.company_id.cmp(&b.company_id)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_grant(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    body: web::Json<CreateGrantRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let body = body.into_inner();
    let result: UnifiedResult<Entitlement> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = admin_caller(&req)?;
        require_capability(&ctx, caller.role.as_deref(), PERM_ENTITLEMENTS_GRANT)?;
        let company_id = validate_company_id(&body.company_id)?;
        let module_id = validate_module_id(&body.module_id)?;
        if ctx.modules.get(&module_id).is_none() {
            return Err(UnifiedError::not_found(format!(
                "Module \"{module_id}\" is not registered"
            )));
        }
        // The tenant namespace id for the row: an explicitly named
        // tenant id, else the company id itself when it is a tenant
        // UUID (the tenants module uses the tenant id as the
        // company id). A non-UUID company with no explicit tenant
        // fails closed with 400 — never a defaulted tenant id.
        let tenant_id = resolve_grant_tenant_id(body.tenant_id.as_deref(), &company_id)?;
        if let Some(expires_at) = body.expires_at {
            if expires_at <= Utc::now() {
                return Err(UnifiedError::bad_request(
                    "expires_at must be in the future",
                ));
            }
        }
        let reason = validate_reason(body.reason.as_deref().unwrap_or(""), true)?;
        // `granted_by` is an audit note only: it names who the
        // request says granted this, but the authorizer is the
        // header identity checked above, never this field.
        let granted_by = body
            .granted_by
            .clone()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty() && value.chars().count() <= MAX_ACTOR_LEN);
        // One active manual grant per (company, module): a second
        // grant replaces the first instead of stacking duplicates.
        // The supersede itself lives in `create_entitlement`, so
        // every grant path (admin, hook, package) dedupes the same
        // way.
        Ok(state.create_entitlement(
            &tenant_id,
            &company_id,
            SOURCE_MANUAL_GRANT,
            "manual",
            None,
            &module_id,
            body.expires_at,
            granted_by.as_deref(),
            reason,
        ))
    })();
    match result {
        Ok(entitlement) => json::success_response(entitlement, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_entitlement(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Entitlement> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = admin_caller(&req)?;
        require_capability(&ctx, caller.role.as_deref(), PERM_ENTITLEMENTS_MANAGE)?;
        state
            .entitlements
            .get(&path.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Entitlement not found"))
    })();
    json::respond(result, debug)
}

/// The only status transitions an admin patch may make. A
/// re-assert of the current status is a no-op, not a transition.
/// Everything else is a 409: a revoked, expired, or disabled row
/// can never be resurrected back to active (a lapsed module
/// returns only through a fresh grant), and a grace row may only
/// return to active via re-grant — never straight to revoked or
/// disabled, which would skip the sweep/grace lifecycle.
pub(crate) fn validate_status_transition(from: &str, to: &str) -> UnifiedResult<()> {
    if from == to {
        return Ok(());
    }
    match (from, to) {
        (STATUS_ACTIVE, STATUS_REVOKED)
        | (STATUS_ACTIVE, STATUS_DISABLED)
        | (STATUS_GRACE, STATUS_ACTIVE) => Ok(()),
        _ => Err(UnifiedError::conflict(format!(
            "Entitlement status cannot move from {from} to {to}"
        ))),
    }
}

pub(crate) async fn update_grant(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    path: web::Path<Uuid>,
    body: web::Json<UpdateGrantRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let body = body.into_inner();
    let result: UnifiedResult<Entitlement> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = admin_caller(&req)?;
        require_capability(&ctx, caller.role.as_deref(), PERM_ENTITLEMENTS_MANAGE)?;
        let actor = caller.actor_id.as_deref();
        let id = path.into_inner();
        let mut row = state
            .entitlements
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Entitlement not found"))?;
        if let Some(status) = body.status {
            let status = status.trim().to_string();
            match status.as_str() {
                STATUS_ACTIVE | STATUS_REVOKED | STATUS_EXPIRED | STATUS_DISABLED
                | STATUS_GRACE => {
                    validate_status_transition(&row.status, &status)?;
                    if status == STATUS_REVOKED && row.status != STATUS_REVOKED {
                        row.lapsed_at = Some(Utc::now());
                        row.lapse_reason = Some("revoked_by_admin".to_string());
                        state.record_history(
                            &row.tenant_id,
                            Some(id),
                            &row.company_id,
                            &row.module_id,
                            "revoked",
                            actor,
                            json!({ "reason": body.reason.as_deref() }),
                        );
                        state.emit(
                            EVENT_ENTITLEMENT_REVOKED,
                            json!({
                                "company_id": row.company_id,
                                "module_id": row.module_id,
                                "source_kind": row.source_kind,
                                "source_id": row.source_id,
                                "entitlement_id": id,
                                "reason": "revoked_by_admin",
                            }),
                        );
                    }
                    if status == STATUS_DISABLED && row.status != STATUS_DISABLED {
                        row.lapsed_at = Some(Utc::now());
                        row.lapse_reason = Some("disabled_by_admin".to_string());
                        state.record_history(
                            &row.tenant_id,
                            Some(id),
                            &row.company_id,
                            &row.module_id,
                            "disabled",
                            actor,
                            json!({ "reason": body.reason.as_deref() }),
                        );
                        state.emit(
                            EVENT_ENTITLEMENT_DISABLED,
                            json!({
                                "company_id": row.company_id,
                                "module_id": row.module_id,
                                "entitlement_id": id,
                                "reason": "disabled_by_admin",
                            }),
                        );
                    }
                    if status == STATUS_ACTIVE && row.status != STATUS_ACTIVE {
                        row.grace_started_at = None;
                        row.grace_ends_at = None;
                        row.lapsed_at = None;
                        row.lapse_reason = None;
                        state.record_history(
                            &row.tenant_id,
                            Some(id),
                            &row.company_id,
                            &row.module_id,
                            "granted",
                            actor,
                            json!({ "reason": body.reason.as_deref() }),
                        );
                        state.emit(
                            EVENT_ENTITLEMENT_GRANTED,
                            json!({
                                "company_id": row.company_id,
                                "module_id": row.module_id,
                                "source_kind": row.source_kind,
                                "source_id": row.source_id,
                                "entitlement_id": id,
                                "expires_at": row.expires_at,
                            }),
                        );
                    }
                    row.status = status;
                }
                _ => {
                    return Err(UnifiedError::bad_request(
                        "status must be one of: active, grace, revoked, expired, disabled",
                    ));
                }
            }
        }
        if let Some(expires_at) = body.expires_at {
            if expires_at <= Utc::now() && row.status == STATUS_ACTIVE {
                return Err(UnifiedError::bad_request(
                    "expires_at must be in the future for an active entitlement",
                ));
            }
            row.expires_at = Some(expires_at);
        }
        if let Some(reason) = body.reason {
            row.reason = validate_reason(&reason, false)?;
        }
        row.updated_at = Utc::now();
        state.entitlements.insert(id, row.clone());
        Ok(row)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_grant(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = admin_caller(&req)?;
        require_capability(&ctx, caller.role.as_deref(), PERM_ENTITLEMENTS_MANAGE)?;
        let id = path.into_inner();
        let row = state
            .entitlements
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Entitlement not found"))?;
        if row.source_kind != SOURCE_MANUAL_GRANT {
            return Err(UnifiedError::conflict(
                "Only manual grants may be deleted; license, subscription, and package entitlements lapse through their own modules",
            ));
        }
        state.entitlements.remove(&id);
        // History is kept: the delete itself is recorded, and the
        // earlier history rows for this entitlement are not removed.
        // Tenant data is never deleted by this call — only the
        // grant row itself.
        state.record_history(
            &row.tenant_id,
            Some(id),
            &row.company_id,
            &row.module_id,
            "grant_deleted",
            caller.actor_id.as_deref(),
            json!({ "source_kind": row.source_kind, "source_id": row.source_id }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_history(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = admin_caller(&req)?;
        require_capability(&ctx, caller.role.as_deref(), PERM_ENTITLEMENTS_MANAGE)?;
        let mut items: Vec<EntitlementHistoryEntry> = state
            .history
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                matches_search(&format!("{} {} {}", entry.company_id, entry.module_id, entry.action), &params)
                    && params
                        .filters
                        .get("company_id")
                        .is_none_or(|company_id| entry.company_id == *company_id)
                    && params
                        .filters
                        .get("module_id")
                        .is_none_or(|module_id| entry.module_id == *module_id)
                    && params
                        .filters
                        .get("action")
                        .is_none_or(|action| entry.action == *action)
                    && params
                        .filters
                        .get("entitlement_id")
                        .is_none_or(|entitlement_id| {
                            entry
                                .entitlement_id
                                .map(|id| id.to_string())
                                .as_deref()
                                == Some(entitlement_id.as_str())
                        })
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_denied_attempts(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = admin_caller(&req)?;
        require_capability(&ctx, caller.role.as_deref(), PERM_ENTITLEMENTS_MANAGE)?;
        let mut items: Vec<DeniedAttempt> = state
            .denied_attempts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|attempt| {
                matches_search(&format!("{} {}", attempt.company_id, attempt.module_id), &params)
                    && params
                        .filters
                        .get("company_id")
                        .is_none_or(|company_id| attempt.company_id == *company_id)
                    && params
                        .filters
                        .get("module_id")
                        .is_none_or(|module_id| attempt.module_id == *module_id)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_settings(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EntitlementSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = admin_caller(&req)?;
        require_capability(&ctx, caller.role.as_deref(), PERM_ENTITLEMENTS_READ)?;
        Ok(state.settings())
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_settings(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    body: web::Json<EntitlementSettings>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let body = body.into_inner();
    let result: UnifiedResult<EntitlementSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = admin_caller(&req)?;
        require_capability(&ctx, caller.role.as_deref(), PERM_ENTITLEMENTS_MANAGE)?;
        body.validate()?;
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = body.clone();
        Ok(body)
    })();
    json::respond(result, debug)
}

pub(crate) async fn run_sweep(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SweepSummary> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = admin_caller(&req)?;
        require_capability(&ctx, caller.role.as_deref(), PERM_ENTITLEMENTS_MANAGE)?;
        Ok(state.sweep())
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
        // The tenant guard requires a resolved tenant context:
        // attach the middleware context the tenancy layer reads.
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
    use unified_core::hooks::{HookBus, HookContext};
    use unified_core::modules::{Module, ModuleRegistry};
    use actix_web::body::to_bytes;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    struct FakeModule {
        id: &'static str,
    }

    impl Module for FakeModule {
        fn id(&self) -> &'static str {
            self.id
        }

        fn name(&self) -> &'static str {
            self.id
        }

        fn description(&self) -> &'static str {
            "test-only module"
        }

        fn requires(&self) -> &[&'static str] {
            &[]
        }
    }

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
        let registry = Arc::new(ModuleRegistry::new());
        registry.register(Arc::new(FakeModule { id: "billing" }));
        let ctx = AppContext::new(AppConfig::default(), hooks, registry);
        web::Data::new(Arc::new(ctx))
    }

    fn admin_request() -> HttpRequest {
        verified_request("admin-2", "admin")
    }

    fn anonymous_request() -> HttpRequest {
        TestRequest::default().to_http_request()
    }

    fn list_query() -> web::Query<PaginationParams> {
        web::Query(PaginationParams::default())
    }

    async fn read_json(resp: HttpResponse) -> (StatusCode, Value) {
        let status = resp.status();
        let bytes = to_bytes(resp.into_body()).await.expect("response body");
        let value: Value = serde_json::from_slice(&bytes).expect("body is JSON");
        (status, value)
    }

    fn error_message(value: &Value) -> String {
        value
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(|message| message.as_str())
            .unwrap_or("")
            .to_string()
    }

    #[tokio::test]
    async fn list_entitlements_rejects_anonymous_callers_with_401() {
        let state = web::Data::new(Arc::new(EntitlementsState::new()));
        let resp = list_entitlements(
            anonymous_request(),
            test_ctx(&[PERM_ENTITLEMENTS_MANAGE]),
            state,
            list_query(),
        )
        .await;
        let (status, body) = read_json(resp).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // The tenant guard now rejects first: no resolved
        // tenant context, before any caller check runs.
        assert!(error_message(&body).contains("did not resolve to a tenant"));
    }

    #[tokio::test]
    async fn list_entitlements_rejects_requests_without_a_verified_caller() {
        let state = web::Data::new(Arc::new(EntitlementsState::new()));
        let resp = list_entitlements(
            TestRequest::default().to_http_request(),
            test_ctx(&[PERM_ENTITLEMENTS_MANAGE]),
            state.clone(),
            list_query(),
        )
        .await;
        let (status, _) = read_json(resp).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_entitlements_rejects_query_param_role_spoof() {
        let state = web::Data::new(Arc::new(EntitlementsState::new()));
        // `?viewer_role=admin` with no verified caller must
        // authorize nothing: the role filter is ignored for
        // identity now.
        let mut params = PaginationParams::default();
        params
            .filters
            .insert("viewer_role".to_string(), "admin".to_string());
        let resp = list_entitlements(
            anonymous_request(),
            test_ctx(&[PERM_ENTITLEMENTS_MANAGE]),
            state,
            web::Query(params),
        )
        .await;
        let (status, _) = read_json(resp).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_entitlements_accepts_a_verified_caller() {
        let state = web::Data::new(Arc::new(EntitlementsState::new()));
        let resp = list_entitlements(
            admin_request(),
            test_ctx(&[PERM_ENTITLEMENTS_MANAGE]),
            state,
            list_query(),
        )
        .await;
        let (status, _) = read_json(resp).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn list_entitlements_rejects_role_without_capability() {
        let state = web::Data::new(Arc::new(EntitlementsState::new()));
        let resp = list_entitlements(
            admin_request(),
            test_ctx(&[PERM_ENTITLEMENTS_READ]),
            state,
            list_query(),
        )
        .await;
        let (status, body) = read_json(resp).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(error_message(&body).contains("entitlements:manage"));
    }

    #[test]
    fn status_transition_table_allows_only_the_documented_moves() {
        // Allowed moves.
        assert!(validate_status_transition(STATUS_ACTIVE, STATUS_REVOKED).is_ok());
        assert!(validate_status_transition(STATUS_ACTIVE, STATUS_DISABLED).is_ok());
        assert!(validate_status_transition(STATUS_GRACE, STATUS_ACTIVE).is_ok());
        // No-op re-assert is not a transition.
        assert!(validate_status_transition(STATUS_ACTIVE, STATUS_ACTIVE).is_ok());
        assert!(validate_status_transition(STATUS_REVOKED, STATUS_REVOKED).is_ok());
        // Resurrection and lifecycle-skipping moves are 409.
        for (from, to) in [
            (STATUS_REVOKED, STATUS_ACTIVE),
            (STATUS_EXPIRED, STATUS_ACTIVE),
            (STATUS_DISABLED, STATUS_ACTIVE),
            (STATUS_ACTIVE, STATUS_EXPIRED),
            (STATUS_ACTIVE, STATUS_GRACE),
            (STATUS_GRACE, STATUS_REVOKED),
            (STATUS_GRACE, STATUS_DISABLED),
            (STATUS_DISABLED, STATUS_REVOKED),
        ] {
            let error = validate_status_transition(from, to).unwrap_err();
            assert_eq!(error.code, 409, "move {from} -> {to} must be 409");
        }
    }

    fn seed_row(state: &EntitlementsState, status: &str) -> Entitlement {
        let company_id = Uuid::now_v7().to_string();
        let row = state.create_entitlement(
            &company_id,
            &company_id,
            SOURCE_MANUAL_GRANT,
            "manual",
            None,
            "billing",
            None,
            Some("admin"),
            Some("seed".to_string()),
        );
        if status != STATUS_ACTIVE {
            let mut updated = row.clone();
            updated.status = status.to_string();
            state.entitlements.insert(row.id, updated.clone());
            return updated;
        }
        row
    }

    fn update_body(status: &str) -> web::Json<UpdateGrantRequest> {
        web::Json(UpdateGrantRequest {
            status: Some(status.to_string()),
            expires_at: None,
            reason: Some("admin decision".to_string()),
        })
    }

    #[tokio::test]
    async fn update_grant_allows_active_to_revoked() {
        let state = Arc::new(EntitlementsState::new());
        let row = seed_row(&state, STATUS_ACTIVE);
        let resp = update_grant(
            admin_request(),
            test_ctx(&[PERM_ENTITLEMENTS_MANAGE]),
            web::Data::new(state.clone()),
            web::Path::from(row.id),
            update_body(STATUS_REVOKED),
        )
        .await;
        let (status, _) = read_json(resp).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            state.entitlements.get(&row.id).unwrap().status,
            STATUS_REVOKED
        );
    }

    #[tokio::test]
    async fn update_grant_rejects_resurrection_with_409() {
        let state = Arc::new(EntitlementsState::new());
        let row = seed_row(&state, STATUS_REVOKED);
        let resp = update_grant(
            admin_request(),
            test_ctx(&[PERM_ENTITLEMENTS_MANAGE]),
            web::Data::new(state.clone()),
            web::Path::from(row.id),
            update_body(STATUS_ACTIVE),
        )
        .await;
        let (status, body) = read_json(resp).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(error_message(&body).contains("cannot move from revoked to active"));
        assert_eq!(
            state.entitlements.get(&row.id).unwrap().status,
            STATUS_REVOKED
        );
    }

    #[tokio::test]
    async fn update_grant_allows_grace_to_active_regrant() {
        let state = Arc::new(EntitlementsState::new());
        let row = seed_row(&state, STATUS_GRACE);
        let resp = update_grant(
            admin_request(),
            test_ctx(&[PERM_ENTITLEMENTS_MANAGE]),
            web::Data::new(state.clone()),
            web::Path::from(row.id),
            update_body(STATUS_ACTIVE),
        )
        .await;
        let (status, _) = read_json(resp).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            state.entitlements.get(&row.id).unwrap().status,
            STATUS_ACTIVE
        );
    }

    #[tokio::test]
    async fn update_grant_rejects_unknown_status_with_400() {
        let state = Arc::new(EntitlementsState::new());
        let row = seed_row(&state, STATUS_ACTIVE);
        let resp = update_grant(
            admin_request(),
            test_ctx(&[PERM_ENTITLEMENTS_MANAGE]),
            web::Data::new(state.clone()),
            web::Path::from(row.id),
            update_body("bogus"),
        )
        .await;
        let (status, _) = read_json(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    fn grant_body(company_id: &str, tenant_id: Option<&str>) -> web::Json<CreateGrantRequest> {
        web::Json(CreateGrantRequest {
            company_id: company_id.to_string(),
            tenant_id: tenant_id.map(str::to_string),
            module_id: "billing".to_string(),
            expires_at: None,
            reason: Some("manual grant".to_string()),
            granted_by: Some("admin-1".to_string()),
        })
    }

    #[tokio::test]
    async fn create_grant_fails_closed_for_non_uuid_company() {
        let state = web::Data::new(Arc::new(EntitlementsState::new()));
        let resp = create_grant(
            admin_request(),
            test_ctx(&[PERM_ENTITLEMENTS_GRANT]),
            state,
            grant_body("acme", None),
        )
        .await;
        let (status, body) = read_json(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error_message(&body).contains("tenant_id is required"));
    }

    #[tokio::test]
    async fn create_grant_uses_uuid_company_as_tenant() {
        let company_id = Uuid::now_v7().to_string();
        let state = web::Data::new(Arc::new(EntitlementsState::new()));
        let resp = create_grant(
            admin_request(),
            test_ctx(&[PERM_ENTITLEMENTS_GRANT]),
            state,
            grant_body(&company_id, None),
        )
        .await;
        let (status, body) = read_json(resp).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(
            body.get("data")
                .and_then(|data| data.get("tenant_id"))
                .and_then(|value| value.as_str()),
            Some(company_id.as_str())
        );
        // `granted_by` from the body is kept as an audit note even
        // though the authorizing actor (header) is `admin-2`.
        assert_eq!(
            body.get("data")
                .and_then(|data| data.get("granted_by"))
                .and_then(|value| value.as_str()),
            Some("admin-1")
        );
    }

    #[tokio::test]
    async fn create_grant_rejects_non_uuid_explicit_tenant() {
        let company_id = Uuid::now_v7().to_string();
        let state = web::Data::new(Arc::new(EntitlementsState::new()));
        let resp = create_grant(
            admin_request(),
            test_ctx(&[PERM_ENTITLEMENTS_GRANT]),
            state,
            grant_body(&company_id, Some("not-a-uuid")),
        )
        .await;
        let (status, _) = read_json(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_grant_replaces_previous_manual_grant() {
        let company_id = Uuid::now_v7().to_string();
        let state = Arc::new(EntitlementsState::new());
        for _ in 0..2 {
            let resp = create_grant(
                admin_request(),
                test_ctx(&[PERM_ENTITLEMENTS_GRANT]),
                web::Data::new(state.clone()),
                grant_body(&company_id, None),
            )
            .await;
            let (status, _) = read_json(resp).await;
            assert_eq!(status, StatusCode::CREATED);
        }
        let active = state.active_rows(&company_id, "billing");
        assert_eq!(active.len(), 1);
    }
}
