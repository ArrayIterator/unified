// Tenant scope and tenant dashboard handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::events;
use unified_core::json;
use unified_core::modules::MODULE_ENTITLEMENT_DENIED_EVENT;
use unified_core::pagination::PaginationParams;
use unified_core::tenancy::TenantContext;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;

use super::*;

// Tenant scope (standing rule: tenant-scoped data access).

/// The tenant a request resolved to, from the canonical tenant
/// context (`unified_core::tenancy::TenantContext`, built by the
/// `TenantResolutionMiddleware` from the Host header through the
/// `tenant.resolve_requested` hook). This is the ONLY trusted
/// tenant identity for tenant-scoped endpoints: `tenant_id` is
/// the uuidv7 namespace, `schema_name` is derived from it ONLY by
/// `tenancy::schema_name()` (never built inline here),
/// `company_id` is the tenant's company key the entitlement rows
/// are keyed by (the tenants module uses the tenant id itself as
/// the company id), and `site_id` names the tenant site. No
/// tenant id, schema name, or package id is ever hard-coded here.
#[derive(Debug, Clone)]
pub struct TenantScope {
    pub tenant_id: String,
    pub schema_name: String,
    pub company_id: String,
    pub site_id: String,
}

impl TenantScope {
    /// Resolve the tenant scope for a tenant-scoped call through
    /// the canonical tenancy extractor. Fail closed: when no
    /// tenant context is present (a control-plane request, an
    /// unresolved host, or middleware not running) the call is
    /// refused with 403 `tenant.required` and no data is returned
    /// - never a silent fall back to the `public` schema and
    /// never another tenant's modules. A `requested` company that
    /// differs from the context's company is also a 403: a tenant
    /// may only ever see its own modules. A resolved context with
    /// no company id is also refused: entitlement rows are keyed
    /// by company, and there is no company to key them by.
    pub fn resolve(req: &HttpRequest, requested: Option<String>) -> UnifiedResult<Self> {
        let denied = || {
            UnifiedError::forbidden(
                "Tenant context is required: this endpoint serves tenant data only",
            )
            .with_metadata("error_code", json!("tenant.required"))
        };
        let context = TenantContext::from_http_request(req).map_err(|_| denied())?;
        let Some(company_id) = context.company_id.clone().filter(|value| !value.trim().is_empty()) else {
            return Err(denied());
        };
        if let Some(requested) = requested {
            let requested = requested.trim().to_string();
            if !requested.is_empty() && requested != company_id {
                return Err(UnifiedError::forbidden(
                    "A tenant may only act on its own company_id",
                )
                .with_metadata("error_code", json!("tenant.required")));
            }
        }
        Ok(Self {
            tenant_id: context.tenant_id.to_string(),
            schema_name: context.schema_name,
            company_id,
            site_id: context.site_id.unwrap_or_default(),
        })
    }
}

// Handlers: tenant dashboard.

pub(crate) async fn list_my_modules(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        // Identity comes from the request headers only: a
        // `?viewer_role=` / `?viewer_id=` query value authorizes
        // nothing (see `caller_of`).
        let caller = caller_of(&req);
        require_capability(&ctx, caller.role.as_deref(), PERM_ENTITLEMENTS_READ)?;
        let scope = TenantScope::resolve(
            &req,
            params.filters.get("company_id").cloned(),
        )?;
        let mut items: Vec<MyModuleView> = Vec::new();
        for module in ctx.modules.list() {
            let module_id = module.id();
            let effective = state.effective(Some(&ctx), &scope.company_id, module_id);
            let upgrade_path = if effective.entitled || !module.available_as_product() {
                UpgradePath {
                    available: false,
                    products: Vec::new(),
                    reason: Some("already_entitled".to_string()),
                }
            } else {
                state.upgrade_path(&scope.company_id, module_id)
            };
            items.push(MyModuleView {
                module_id: module_id.to_string(),
                name: module.name().to_string(),
                description: module.description().to_string(),
                available_as_product: module.available_as_product(),
                state: effective.state,
                entitled: effective.entitled,
                read_only: effective.read_only,
                write_allowed: effective.write_allowed,
                enabled: effective.enabled,
                sources: effective.sources,
                expires_at: effective.expires_at,
                grace_ends_at: effective.grace_ends_at,
                upgrade_path,
            });
        }
        items.retain(|item| {
            matches_search(&format!("{} {} {}", item.module_id, item.name, item.description), &params)
                && params
                    .filters
                    .get("state")
                    .is_none_or(|state| item.state == *state)
                && params
                    .filters
                    .get("entitled")
                    .is_none_or(|entitled| item.entitled == (*entitled == "true" || *entitled == "1"))
                && params
                    .filters
                    .get("available_as_product")
                    .is_none_or(|value| {
                        item.available_as_product == (*value == "true" || *value == "1")
                    })
        });
        let sort = params.allowed_sort(&["module_id", "name", "state"], "module_id");
        match sort.as_str() {
            "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
            "state" => items.sort_by(|a, b| {
                a.state.cmp(&b.state).then(a.module_id.cmp(&b.module_id))
            }),
            _ => items.sort_by(|a, b| a.module_id.cmp(&b.module_id)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(json!({
            "tenant_id": scope.tenant_id,
            "schema_name": scope.schema_name,
            "company_id": scope.company_id,
            "site_id": scope.site_id,
            "modules": paginate(items, &params).to_json(),
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_module_access(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    path: web::Path<String>,
    query: web::Query<ViewerQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let module_id = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        // Identity comes from the request headers only; the query
        // carries the requested company, never the caller.
        let caller = caller_of(&req);
        require_capability(&ctx, caller.role.as_deref(), PERM_ENTITLEMENTS_READ)?;
        let scope = TenantScope::resolve(&req, query.company_id.clone())?;
        let module_id = validate_module_id(&module_id)?;
        let module = ctx
            .modules
            .get(&module_id)
            .ok_or_else(|| UnifiedError::not_found(format!("Module \"{module_id}\" is not registered")))?;
        let effective = state.effective(Some(&ctx), &scope.company_id, &module_id);
        let upgrade_path = if effective.entitled || !module.available_as_product() {
            UpgradePath {
                available: false,
                products: Vec::new(),
                reason: Some("already_entitled".to_string()),
            }
        } else {
            state.upgrade_path(&scope.company_id, &module_id)
        };
        Ok(json!({
            "tenant_id": scope.tenant_id,
            "schema_name": scope.schema_name,
            "company_id": scope.company_id,
            "site_id": scope.site_id,
            "access": effective,
            "upgrade_path": upgrade_path,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn enable_module(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    path: web::Path<String>,
    body: web::Json<EnableModuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let module_id = path.into_inner();
    let body = body.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        // Identity comes from the request headers only.
        let caller = caller_of(&req);
        // Enabling mutates tenant activation state, so it needs
        // the manage capability - a read-only role must not be
        // able to enable modules.
        require_capability(&ctx, caller.role.as_deref(), PERM_ENTITLEMENTS_MANAGE)?;
        // The recorded actor is the header caller. A body
        // `actor_id` naming someone else is allowed here only
        // because the manage capability was already required
        // above (admin/on-behalf flow).
        let actor_id = caller.actor_id.clone().or(body.actor_id.clone());
        let actor = actor_id.as_deref();
        let scope = TenantScope::resolve(&req, body.company_id.clone())?;
        let module_id = validate_module_id(&module_id)?;
        let module = ctx
            .modules
            .get(&module_id)
            .ok_or_else(|| UnifiedError::not_found(format!("Module \"{module_id}\" is not registered")))?;
        // Non-sellable modules need no entitlement (the licensing
        // gate's rule, composed here rather than duplicated).
        if !module.available_as_product() {
            set_activation(&state, &scope, &module_id, true, actor, None, None);
            state.emit(
                events::MODULE_ENABLED,
                json!({ "id": module_id, "company_id": scope.company_id, "reason": "not_sellable" }),
            );
            let effective = state.effective(Some(&ctx), &scope.company_id, &module_id);
            return Ok(json!({ "tenant_id": scope.tenant_id, "access": effective }));
        }
        let effective = state.effective(Some(&ctx), &scope.company_id, &module_id);
        // Enabling is a write, so only full entitlement enables:
        // `effective.entitled` is the one decision (see the grace
        // policy on `EntitlementsState::grace_applies`). Grace is
        // read-only and never enables - a grace-state module
        // enabled here would be an enabled module the licensing
        // gate 403s, the exact divergence this used to be.
        if effective.entitled {
            set_activation(&state, &scope, &module_id, true, actor, None, None);
            state.emit(
                events::MODULE_ENABLED,
                json!({ "id": module_id, "company_id": scope.company_id, "reason": effective.state }),
            );
            let effective = state.effective(Some(&ctx), &scope.company_id, &module_id);
            Ok(json!({ "tenant_id": scope.tenant_id, "access": effective }))
        } else {
            // Grace reads as a distinct denial, not "no active
            // entitlement": the tenant held the module and it
            // lapsed, so renewal - not a new purchase - is the
            // way back. Locked modules keep the existing code.
            let (reason, error_code, denied_reason) = if effective.read_only {
                (
                    format!(
                        "Module \"{module_id}\" is in read-only grace for this company: renew the entitlement to re-enable it"
                    ),
                    ERROR_ENTITLEMENT_GRACE_READ_ONLY,
                    "grace_read_only",
                )
            } else {
                (
                    format!(
                        "Module \"{module_id}\" requires an active entitlement for this company"
                    ),
                    ERROR_ENTITLEMENT_REQUIRED,
                    "no_active_entitlement",
                )
            };
            state.record_denied(
                &scope.tenant_id,
                &scope.company_id,
                &module_id,
                actor,
                &reason,
                error_code,
            );
            state.emit(
                EVENT_ENTITLEMENT_ENABLE_DENIED,
                json!({
                    "company_id": scope.company_id,
                    "module_id": module_id,
                    "actor_id": actor,
                    "reason": reason,
                    "error_code": error_code,
                }),
            );
            state.emit(
                MODULE_ENTITLEMENT_DENIED_EVENT,
                json!({
                    "id": module_id,
                    "company_id": scope.company_id,
                    "reason": denied_reason,
                }),
            );
            Err(UnifiedError::forbidden(reason)
                .with_metadata("error_code", json!(error_code))
                .with_metadata("module_id", json!(module_id))
                .with_metadata("company_id", json!(scope.company_id)))
        }
    })();
    json::respond(result, debug)
}

pub(crate) async fn disable_module(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    path: web::Path<String>,
    body: web::Json<EnableModuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let module_id = path.into_inner();
    let body = body.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        // Identity comes from the request headers only.
        let caller = caller_of(&req);
        // Disabling mutates tenant activation state, so it needs
        // the manage capability - a read-only role must not be
        // able to disable modules.
        require_capability(&ctx, caller.role.as_deref(), PERM_ENTITLEMENTS_MANAGE)?;
        // The recorded actor is the header caller. A body
        // `actor_id` naming someone else is allowed here only
        // because the manage capability was already required
        // above (admin/on-behalf flow).
        let actor_id = caller.actor_id.clone().or(body.actor_id.clone());
        let actor = actor_id.as_deref();
        let scope = TenantScope::resolve(&req, body.company_id.clone())?;
        let module_id = validate_module_id(&module_id)?;
        if ctx.modules.get(&module_id).is_none() {
            return Err(UnifiedError::not_found(format!(
                "Module \"{module_id}\" is not registered"
            )));
        }
        set_activation(&state, &scope, &module_id, false, actor, None, None);
        state.record_history(
            &scope.tenant_id,
            None,
            &scope.company_id,
            &module_id,
            "disabled",
            actor,
            json!({ "reason": "disabled_by_tenant" }),
        );
        state.emit(
            events::MODULE_DISABLED,
            json!({ "id": module_id, "company_id": scope.company_id, "reason": "disabled_by_tenant" }),
        );
        let effective = state.effective(Some(&ctx), &scope.company_id, &module_id);
        Ok(json!({ "tenant_id": scope.tenant_id, "access": effective }))
    })();
    json::respond(result, debug)
}

/// Record (or update) the per-tenant activation state for a
/// module. Activation state IS tenant business data: it is written
/// only under a resolved tenant scope, keyed by the tenant's
/// company, and never under a control-plane identity.
pub(crate) fn set_activation(
    state: &EntitlementsState,
    scope: &TenantScope,
    module_id: &str,
    enabled: bool,
    enabled_by: Option<&str>,
    source_kind: Option<&str>,
    source_id: Option<&str>,
) {
    let now = Utc::now();
    let key = EntitlementsState::activation_key(&scope.company_id, module_id);
    state.activations.insert(
        key,
        ModuleActivation {
            tenant_id: scope.tenant_id.clone(),
            company_id: scope.company_id.clone(),
            module_id: module_id.to_string(),
            enabled,
            enabled_at: now,
            enabled_by: enabled_by.map(str::to_string),
            source_kind: source_kind.map(str::to_string),
            source_id: source_id.map(str::to_string),
            updated_at: now,
        },
    );
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
    use unified_core::hooks::{HookBus, HookContext};
    use unified_core::middleware::{TenantContext as ResolvedTenantContext, TenantKind};
    use unified_core::modules::{Module, ModuleRegistry};
    use actix_web::body::to_bytes;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;
    use uuid::Uuid;

    /// A minimal registry module for tests: `billing` is a
    /// sellable non-core module, so only a stored row (or a
    /// license) entitles it - never the implicit owner leg.
    struct FakeModule {
        id: &'static str,
        core: bool,
        sellable: bool,
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

        fn available_as_product(&self) -> bool {
            self.sellable
        }

        fn is_core(&self) -> bool {
            self.core
        }
    }

    /// App context whose `role.check` hook allows exactly the
    /// given permissions (all other permissions are denied), with
    /// `billing` registered as a sellable non-core module.
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
        registry.register(Arc::new(FakeModule {
            id: "billing",
            core: false,
            sellable: true,
        }));
        let ctx = AppContext::new(AppConfig::default(), hooks, registry);
        web::Data::new(Arc::new(ctx))
    }

    fn plain_request() -> HttpRequest {
        verified_request("tester", "admin")
    }

    /// A request that passed tenant resolution: the middleware
    /// `TenantContext` is attached to the request extensions, with
    /// the tenant id itself as the company id (the tenants-module
    /// convention).
    fn tenant_request(company_id: &str) -> HttpRequest {
        let req = plain_request();
        req.extensions_mut().insert(ResolvedTenantContext {
            tenant_id: company_id.to_string(),
            company_id: company_id.to_string(),
            site_id: "site-1".to_string(),
            kind: TenantKind::Subdomain,
            host: "tenant.example.com".to_string(),
            scope: None,
            product_id: None,
            package_id: None,
            product_subdomain_id: None,
        });
        req
    }

    fn enable_body() -> web::Json<EnableModuleRequest> {
        web::Json(EnableModuleRequest {
            company_id: None,
            actor_id: Some("tester".to_string()),
        })
    }

    async fn error_envelope(resp: HttpResponse) -> (StatusCode, Value) {
        let status = resp.status();
        let bytes = to_bytes(resp.into_body()).await.expect("response body");
        let value: Value = serde_json::from_slice(&bytes).expect("error body is JSON");
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

    fn error_code(value: &Value) -> String {
        value
            .get("error")
            .and_then(|error| error.get("metadata"))
            .and_then(|metadata| metadata.get("error_code"))
            .and_then(|code| code.as_str())
            .unwrap_or("")
            .to_string()
    }

    #[tokio::test]
    async fn enable_module_rejects_read_only_role() {
        let state = web::Data::new(Arc::new(EntitlementsState::new()));
        let resp = enable_module(
            tenant_request(&Uuid::now_v7().to_string()),
            test_ctx(&[PERM_ENTITLEMENTS_READ]),
            state,
            web::Path::from("billing".to_string()),
            enable_body(),
        )
        .await;
        let (status, body) = error_envelope(resp).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(error_message(&body).contains("entitlements:manage"));
    }

    #[tokio::test]
    async fn disable_module_rejects_read_only_role() {
        let state = web::Data::new(Arc::new(EntitlementsState::new()));
        let resp = disable_module(
            tenant_request(&Uuid::now_v7().to_string()),
            test_ctx(&[PERM_ENTITLEMENTS_READ]),
            state,
            web::Path::from("billing".to_string()),
            enable_body(),
        )
        .await;
        let (status, body) = error_envelope(resp).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(error_message(&body).contains("entitlements:manage"));
    }

    #[tokio::test]
    async fn enable_module_with_manage_rejects_missing_tenant_context() {
        let state = web::Data::new(Arc::new(EntitlementsState::new()));
        let resp = enable_module(
            plain_request(),
            test_ctx(&[PERM_ENTITLEMENTS_MANAGE]),
            state,
            web::Path::from("billing".to_string()),
            enable_body(),
        )
        .await;
        let (status, body) = error_envelope(resp).await;
        // The tenant guard rejects first: no resolved tenant
        // context, before tenant scope is ever resolved.
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(error_message(&body).contains("did not resolve to a tenant"));
    }

    #[tokio::test]
    async fn enable_module_denied_after_package_downgrade() {
        let company_id = Uuid::now_v7().to_string();
        let state = Arc::new(EntitlementsState::new());
        // Grace disabled, so a lapsed row reads as locked, not
        // grace_read_only.
        *state.settings.write().expect("settings lock poisoned") = EntitlementSettings {
            grace_period_days: 0,
            read_only_grace_enabled: false,
            default_deny: true,
        };
        state.create_entitlement(
            &company_id,
            &company_id,
            SOURCE_SAAS_PACKAGE,
            "pkg-old",
            None,
            "billing",
            None,
            Some("admin"),
            None,
        );
        assert_eq!(
            state.effective(None, &company_id, "billing").state,
            ACCESS_ENTITLED
        );
        // The downgrade: the package source lapses, so no active
        // row remains and the module reads as locked.
        state.apply_source_lapse(
            SOURCE_SAAS_PACKAGE,
            "pkg-old",
            Some(&company_id),
            "package_change",
        );
        assert_eq!(
            state.effective(None, &company_id, "billing").state,
            ACCESS_LOCKED
        );
        let resp = enable_module(
            tenant_request(&company_id),
            test_ctx(&[PERM_ENTITLEMENTS_MANAGE]),
            web::Data::new(state),
            web::Path::from("billing".to_string()),
            enable_body(),
        )
        .await;
        let (status, body) = error_envelope(resp).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(error_code(&body), ERROR_ENTITLEMENT_REQUIRED);
    }

    #[tokio::test]
    async fn enable_module_denied_in_read_only_grace() {
        let company_id = Uuid::now_v7().to_string();
        let state = Arc::new(EntitlementsState::new());
        // Grace enabled (the default): the lapsed row reads as
        // grace_read_only, and grace never enables - the module
        // cannot be re-enabled until a live source re-entitles it.
        state.create_entitlement(
            &company_id,
            &company_id,
            SOURCE_SAAS_PACKAGE,
            "pkg-old",
            None,
            "billing",
            None,
            Some("admin"),
            None,
        );
        state.apply_source_lapse(
            SOURCE_SAAS_PACKAGE,
            "pkg-old",
            Some(&company_id),
            "package_change",
        );
        assert_eq!(
            state.effective(None, &company_id, "billing").state,
            ACCESS_GRACE_READ_ONLY
        );
        let resp = enable_module(
            tenant_request(&company_id),
            test_ctx(&[PERM_ENTITLEMENTS_MANAGE]),
            web::Data::new(state),
            web::Path::from("billing".to_string()),
            enable_body(),
        )
        .await;
        let (status, body) = error_envelope(resp).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(error_code(&body), ERROR_ENTITLEMENT_GRACE_READ_ONLY);
    }

    #[tokio::test]
    async fn list_my_modules_rejects_query_param_role_spoof() {
        let state = web::Data::new(Arc::new(EntitlementsState::new()));
        // `?viewer_role=admin` / `?viewer_id=` with no tenant
        // context must authorize nothing: the tenant guard
        // rejects first.
        let mut params = PaginationParams::default();
        params
            .filters
            .insert("viewer_role".to_string(), "admin".to_string());
        params
            .filters
            .insert("viewer_id".to_string(), "spoofed".to_string());
        let resp = list_my_modules(
            TestRequest::default().to_http_request(),
            test_ctx(&[PERM_ENTITLEMENTS_READ]),
            state,
            web::Query(params),
        )
        .await;
        let (status, _) = error_envelope(resp).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn get_module_access_rejects_query_param_role_spoof() {
        let state = web::Data::new(Arc::new(EntitlementsState::new()));
        let resp = get_module_access(
            TestRequest::default()
                .uri("/api/v1/entitlements/modules/billing/access?viewer_role=admin")
                .to_http_request(),
            test_ctx(&[PERM_ENTITLEMENTS_READ]),
            state,
            web::Path::from("billing".to_string()),
            web::Query(ViewerQuery {
                company_id: None,
            }),
        )
        .await;
        let (status, _) = error_envelope(resp).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_my_modules_accepts_a_verified_caller() {
        let company_id = Uuid::now_v7().to_string();
        let state = web::Data::new(Arc::new(EntitlementsState::new()));
        let resp = list_my_modules(
            tenant_request(&company_id),
            test_ctx(&[PERM_ENTITLEMENTS_READ]),
            state,
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
