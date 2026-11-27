// Handlers: package / product-set changes.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::events;
use unified_core::json;
use unified_core::pagination::PaginationParams;
use crate::modules::licensing::has_module_entitlement;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

use super::*;

// Handlers: package / product-set changes.

pub(crate) async fn apply_package_change(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    body: web::Json<ApplyPackageChangeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let body = body.into_inner();
    let result: UnifiedResult<PackageChange> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let role = unified_core::caller::caller_from(&req)
            .roles
            .into_iter()
            .next();
        require_capability(&ctx, role.as_deref(), PERM_ENTITLEMENTS_MANAGE)?;
        let scope = TenantScope::resolve(&req, Some(body.company_id.clone()))?;
        let source_kind = match body.source_kind {
            Some(kind) => validate_source_kind(&kind)?,
            None => SOURCE_SAAS_PACKAGE.to_string(),
        };
        let source_id = match body.source_id {
            Some(id) => validate_source_id(&id)?,
            None => {
                return Err(UnifiedError::bad_request(
                    "source_id is required (package, product, subscription, or license id)",
                ));
            }
        };
        let new_module_ids = validate_module_id_list(&body.new_module_ids)?;
        for module_id in &new_module_ids {
            if ctx.modules.get(module_id).is_none() {
                return Err(UnifiedError::not_found(format!(
                    "Module \"{module_id}\" is not registered"
                )));
            }
        }
        if let Some(expires_at) = body.expires_at {
            if expires_at <= Utc::now() {
                return Err(UnifiedError::bad_request(
                    "expires_at must be in the future",
                ));
            }
        }
        // Only sellable modules take part in the diff: a module
        // that is not sellable needs no entitlement and is kept
        // unconditionally.
        let (sellable_new, free_new): (Vec<String>, Vec<String>) = new_module_ids
            .into_iter()
            .partition(|module_id| {
                ctx.modules
                    .get(module_id)
                    .map(|module| module.available_as_product())
                    .unwrap_or(false)
            });
        let current = state.entitled_module_ids(Some(&ctx), &scope.company_id);
        let current_set: HashSet<String> = current.iter().cloned().collect();
        let new_set: HashSet<String> = sellable_new.iter().cloned().collect();
        let mut newly_entitled: Vec<String> = new_set
            .difference(&current_set)
            .cloned()
            .collect();
        newly_entitled.sort();
        let mut kept: Vec<String> = new_set
            .intersection(&current_set)
            .cloned()
            .collect();
        kept.extend(free_new);
        kept.sort();
        let mut lost: Vec<String> = current_set
            .difference(&new_set)
            .cloned()
            .collect();
        lost.sort();
        let now = Utc::now();
        let mut kept_via_other_source: Vec<String> = Vec::new();
        let mut requires_confirmation: Vec<String> = Vec::new();
        let mut disabled_module_ids: Vec<String> = Vec::new();
        for module_id in lost {
            // Still covered by another active source (a different
            // license, subscription, package, or grant) or by the
            // licensing leg: the module stays entitled and is never
            // listed for confirmation.
            let other_stored = state.entitlements.iter().any(|entry| {
                let row = entry.value();
                row.company_id == scope.company_id
                    && row.module_id == module_id
                    && row.status == STATUS_ACTIVE
                    && row.expires_at.is_none_or(|expires| expires > now)
                    && !(row.source_kind == source_kind && row.source_id == source_id)
            });
            let other_licensed =
                has_module_entitlement(&ctx, &scope.company_id, &module_id);
            if other_stored || other_licensed {
                kept_via_other_source.push(module_id);
                continue;
            }
            // No other source: never silently disable when the
            // tenant uses the module outside the package. When
            // nothing answers the usage query the module is
            // treated as used (fail-safe).
            if state.used_outside_package(&scope.company_id, &module_id) {
                requires_confirmation.push(module_id);
            } else {
                // The downgrade must revoke the entitlement, not
                // just the activation flag: `effective()` ignores
                // the activation flag and reads any active row as
                // entitled, so leaving the rows active would let
                // the tenant re-enable the dropped module. Lapse
                // the rows of the replaced source (the source
                // named in this change - a package change re-applies
                // the same source with a new module set); a module
                // covered by another source never reaches this
                // branch, and `apply_source_lapse` re-checks that
                // coverage before lapsing. The lapse moves the rows
                // to grace (or disabled when grace is off); manual
                // grants are never a package source, so none is
                // touched here.
                state.apply_source_lapse(
                    &source_kind,
                    &source_id,
                    Some(&scope.company_id),
                    "package_change",
                );
                set_activation(&state, &scope, &module_id, false, body.requested_by.as_deref(), Some(&source_kind), Some(&source_id));
                state.record_history(
                    &scope.tenant_id,
                    None,
                    &scope.company_id,
                    &module_id,
                    "disabled",
                    body.requested_by.as_deref(),
                    json!({ "reason": "package_change", "source_kind": source_kind, "source_id": source_id }),
                );
                state.emit(
                    events::MODULE_DISABLED,
                    json!({ "id": module_id, "company_id": scope.company_id, "reason": "package_change" }),
                );
                disabled_module_ids.push(module_id);
            }
        }
        // Newly entitled modules are enabled under the new source.
        for module_id in &newly_entitled {
            state.create_entitlement(
                &scope.tenant_id,
                &scope.company_id,
                &source_kind,
                &source_id,
                body.source_ref.clone(),
                module_id,
                body.expires_at,
                body.requested_by.as_deref(),
                None,
            );
            set_activation(
                &state,
                &scope,
                module_id,
                true,
                body.requested_by.as_deref(),
                Some(&source_kind),
                Some(&source_id),
            );
            state.emit(
                events::MODULE_ENABLED,
                json!({ "id": module_id, "company_id": scope.company_id, "reason": "package_change" }),
            );
        }
        let status = if requires_confirmation.is_empty() {
            "applied"
        } else {
            "pending_confirmation"
        };
        let change = PackageChange {
            id: Uuid::now_v7(),
            tenant_id: scope.tenant_id.clone(),
            company_id: scope.company_id.clone(),
            source_kind,
            source_id,
            new_module_ids: sellable_new,
            newly_entitled,
            kept,
            kept_via_other_source,
            requires_confirmation,
            disabled_module_ids,
            status: status.to_string(),
            requested_by: body.requested_by.clone(),
            decided_by: None,
            decided_at: None,
            created_at: now,
            updated_at: now,
        };
        state
            .package_changes
            .insert(change.id, change.clone());
        Ok(change)
    })();
    match result {
        Ok(change) => json::success_response(change, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_package_changes(
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
        // Identity comes from the server-verified caller only: a
        // `?viewer_role=` query value authorizes nothing.
        let role = unified_core::caller::caller_from(&req)
            .roles
            .into_iter()
            .next();
        require_capability(&ctx, role.as_deref(), PERM_ENTITLEMENTS_READ)?;
        let scope = TenantScope::resolve(&req, params.filters.get("company_id").cloned())?;
        let mut items: Vec<PackageChange> = state
            .package_changes
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|change| {
                change.company_id == scope.company_id
                    && matches_search(
                        &format!("{} {}", change.source_kind, change.source_id),
                        &params,
                    )
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| change.status == *status)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_package_change(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PackageChange> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let role = unified_core::caller::caller_from(&req)
            .roles
            .into_iter()
            .next();
        require_capability(&ctx, role.as_deref(), PERM_ENTITLEMENTS_READ)?;
        let scope = TenantScope::resolve(&req, None)?;
        let change = state
            .package_changes
            .get(&path.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Package change not found"))?;
        if change.company_id != scope.company_id {
            return Err(UnifiedError::not_found("Package change not found"));
        }
        Ok(change)
    })();
    json::respond(result, debug)
}

pub(crate) async fn confirm_package_change(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    path: web::Path<Uuid>,
    body: web::Json<ConfirmPackageChangeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let body = body.into_inner();
    let result: UnifiedResult<PackageChange> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let role = unified_core::caller::caller_from(&req)
            .roles
            .into_iter()
            .next();
        // Confirming mutates tenant activation and entitlement
        // rows, so it needs the manage capability - a read-only
        // role must not be able to confirm (or discard) a change.
        require_capability(&ctx, role.as_deref(), PERM_ENTITLEMENTS_MANAGE)?;
        let scope = TenantScope::resolve(&req, None)?;
        confirm_package_change_inner(&state, &scope, path.into_inner(), body)
    })();
    json::respond(result, debug)
}

/// The confirm itself, separated from the HTTP wrapper so the
/// race guard is unit-testable. The whole confirm runs under
/// `state.confirm_lock`: without it two concurrent confirms
/// could both load the change as `pending_confirmation`, both
/// disable the same modules, and both push into
/// `disabled_module_ids`. Under the lock the second confirm
/// loads the settled status and gets a 409. The push is also
/// de-duplicated, so a module already recorded as disabled is
/// never listed twice.
pub(crate) fn confirm_package_change_inner(
    state: &EntitlementsState,
    scope: &TenantScope,
    id: Uuid,
    body: ConfirmPackageChangeRequest,
) -> UnifiedResult<PackageChange> {
    let _confirm_guard = state.confirm_lock.lock().expect("confirm lock poisoned");
    let mut change = state
        .package_changes
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Package change not found"))?;
    if change.company_id != scope.company_id {
        return Err(UnifiedError::not_found("Package change not found"));
    }
    if change.status != "pending_confirmation" {
        return Err(UnifiedError::conflict(format!(
            "Package change is already {}",
            change.status
        )));
    }
    let decided_by = body.decided_by.clone();
    if body.confirm != Some(true) {
        change.status = "discarded".to_string();
        change.decided_by = decided_by.clone();
        change.decided_at = Some(Utc::now());
        change.updated_at = Utc::now();
        state.package_changes.insert(id, change.clone());
        state.record_history(
            &scope.tenant_id,
            None,
            &scope.company_id,
            "",
            "package_change_discarded",
            decided_by.as_deref(),
            json!({ "change_id": id, "reason": body.reason }),
        );
        return Ok(change);
    }
    let wanted: Vec<String> = match body.module_ids {
        Some(ids) => validate_module_id_list(&ids)?,
        None => change.requires_confirmation.clone(),
    };
    for module_id in &wanted {
        if !change.requires_confirmation.contains(module_id) {
            return Err(UnifiedError::bad_request(format!(
                "Module \"{module_id}\" is not listed as requires_confirmation for this change"
            )));
        }
    }
    for module_id in wanted {
        // Same revocation rule as the apply path: confirming the
        // loss must lapse the replaced source's rows, not just
        // the activation flag, or the module stays entitled and
        // re-enableable.
        state.apply_source_lapse(
            &change.source_kind,
            &change.source_id,
            Some(&scope.company_id),
            "package_change_confirmed",
        );
        set_activation(
            state,
            scope,
            &module_id,
            false,
            decided_by.as_deref(),
            Some(&change.source_kind),
            Some(&change.source_id),
        );
        state.record_history(
            &scope.tenant_id,
            None,
            &scope.company_id,
            &module_id,
            "disabled",
            decided_by.as_deref(),
            json!({ "reason": "package_change_confirmed", "change_id": id }),
        );
        state.emit(
            events::MODULE_DISABLED,
            json!({ "id": module_id, "company_id": scope.company_id, "reason": "package_change_confirmed" }),
        );
        if !change.disabled_module_ids.contains(&module_id) {
            change.disabled_module_ids.push(module_id);
        }
    }
    change
        .requires_confirmation
        .retain(|module_id| !change.disabled_module_ids.contains(module_id));
    change.status = if change.requires_confirmation.is_empty() {
        "confirmed".to_string()
    } else {
        "pending_confirmation".to_string()
    };
    change.decided_by = decided_by;
    change.decided_at = Some(Utc::now());
    change.updated_at = Utc::now();
    state.package_changes.insert(id, change.clone());
    Ok(change)
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
    use unified_core::modules::ModuleRegistry;
    use actix_web::body::to_bytes;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

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
        let ctx = AppContext::new(AppConfig::default(), hooks, Arc::new(ModuleRegistry::new()));
        web::Data::new(Arc::new(ctx))
    }

    fn plain_request() -> HttpRequest {
        verified_request("tester", "admin")
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

    fn apply_body() -> web::Json<ApplyPackageChangeRequest> {
        web::Json(ApplyPackageChangeRequest {
            company_id: Uuid::now_v7().to_string(),
            source_kind: None,
            source_id: Some("pkg-1".to_string()),
            source_ref: None,
            new_module_ids: Vec::new(),
            expires_at: None,
            requested_by: Some("tester".to_string()),
        })
    }

    fn confirm_body() -> web::Json<ConfirmPackageChangeRequest> {
        web::Json(ConfirmPackageChangeRequest {
            confirm: Some(true),
            module_ids: None,
            decided_by: Some("tester".to_string()),
            reason: None,
        })
    }

    #[tokio::test]
    async fn apply_package_change_rejects_read_only_role() {
        let state = web::Data::new(Arc::new(EntitlementsState::new()));
        let resp = apply_package_change(
            tenant_request(&Uuid::now_v7().to_string()),
            test_ctx(&[PERM_ENTITLEMENTS_READ]),
            state,
            apply_body(),
        )
        .await;
        let (status, body) = error_envelope(resp).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(error_message(&body).contains("entitlements:manage"));
    }

    #[tokio::test]
    async fn apply_package_change_with_manage_rejects_missing_tenant_context() {
        let state = web::Data::new(Arc::new(EntitlementsState::new()));
        let resp = apply_package_change(
            plain_request(),
            test_ctx(&[PERM_ENTITLEMENTS_MANAGE]),
            state,
            apply_body(),
        )
        .await;
        let (status, body) = error_envelope(resp).await;
        // The tenant guard rejects first: no resolved tenant
        // context, before tenant scope is ever resolved.
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(error_message(&body).contains("did not resolve to a tenant"));
    }

    #[tokio::test]
    async fn confirm_package_change_rejects_read_only_role() {
        let state = web::Data::new(Arc::new(EntitlementsState::new()));
        let resp = confirm_package_change(
            tenant_request(&Uuid::now_v7().to_string()),
            test_ctx(&[PERM_ENTITLEMENTS_READ]),
            state,
            web::Path::from(Uuid::now_v7()),
            confirm_body(),
        )
        .await;
        let (status, body) = error_envelope(resp).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(error_message(&body).contains("entitlements:manage"));
    }

    fn seeded_change(state: &EntitlementsState, company_id: &str) -> (TenantScope, Uuid) {
        *state.settings.write().expect("settings lock poisoned") = EntitlementSettings {
            grace_period_days: 0,
            read_only_grace_enabled: false,
            default_deny: true,
        };
        state.create_entitlement(
            company_id,
            company_id,
            SOURCE_SAAS_PACKAGE,
            "pkg-old",
            None,
            "billing",
            None,
            Some("admin"),
            None,
        );
        let id = Uuid::now_v7();
        let now = Utc::now();
        state.package_changes.insert(
            id,
            PackageChange {
                id,
                tenant_id: company_id.to_string(),
                company_id: company_id.to_string(),
                source_kind: SOURCE_SAAS_PACKAGE.to_string(),
                source_id: "pkg-old".to_string(),
                new_module_ids: Vec::new(),
                newly_entitled: Vec::new(),
                kept: Vec::new(),
                kept_via_other_source: Vec::new(),
                requires_confirmation: vec!["billing".to_string()],
                disabled_module_ids: Vec::new(),
                status: "pending_confirmation".to_string(),
                requested_by: Some("admin".to_string()),
                decided_by: None,
                decided_at: None,
                created_at: now,
                updated_at: now,
            },
        );
        let scope = TenantScope {
            tenant_id: company_id.to_string(),
            schema_name: unified_core::tenancy::schema_name(
                Uuid::parse_str(company_id).expect("company id is a UUID"),
            ),
            company_id: company_id.to_string(),
            site_id: "site-1".to_string(),
        };
        (scope, id)
    }

    fn confirm_request() -> ConfirmPackageChangeRequest {
        ConfirmPackageChangeRequest {
            confirm: Some(true),
            module_ids: None,
            decided_by: Some("tester".to_string()),
            reason: None,
        }
    }

    #[test]
    fn concurrent_confirms_settle_once_without_duplicate_disables() {
        let state = Arc::new(EntitlementsState::new());
        let company_id = Uuid::now_v7().to_string();
        let (scope, id) = seeded_change(&state, &company_id);
        let (first, second) = std::thread::scope(|thread_scope| {
            let first = thread_scope
                .spawn(|| confirm_package_change_inner(&state, &scope, id, confirm_request()));
            let second = thread_scope
                .spawn(|| confirm_package_change_inner(&state, &scope, id, confirm_request()));
            (
                first.join().expect("first confirm thread"),
                second.join().expect("second confirm thread"),
            )
        });
        // Exactly one confirm wins; the loser sees the settled
        // status and gets a 409 instead of disabling twice.
        assert_eq!(first.is_ok(), !second.is_ok());
        let loser = if first.is_err() { &first } else { &second };
        assert_eq!(loser.as_ref().unwrap_err().code, 409);
        let stored = state
            .package_changes
            .get(&id)
            .map(|entry| entry.value().clone())
            .expect("change still stored");
        assert_eq!(stored.status, "confirmed");
        assert_eq!(stored.disabled_module_ids, vec!["billing".to_string()]);
        assert!(state.active_rows(&company_id, "billing").is_empty());
        assert_eq!(
            state.effective(None, &company_id, "billing").state,
            ACCESS_LOCKED
        );
    }

    #[test]
    fn second_confirm_after_settled_status_is_conflict() {
        let state = EntitlementsState::new();
        let company_id = Uuid::now_v7().to_string();
        let (scope, id) = seeded_change(&state, &company_id);
        let confirmed = confirm_package_change_inner(&state, &scope, id, confirm_request())
            .expect("first confirm succeeds");
        assert_eq!(confirmed.status, "confirmed");
        let error =
            confirm_package_change_inner(&state, &scope, id, confirm_request()).unwrap_err();
        assert_eq!(error.code, 409);
    }

    /// A request that passed tenant resolution: the middleware
    /// `TenantContext` is attached to the request extensions, with
    /// the tenant id itself as the company id (the tenants-module
    /// convention).
    fn tenant_request(company_id: &str) -> HttpRequest {
        use unified_core::middleware::{
            TenantContext as ResolvedTenantContext, TenantKind,
        };
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

    #[tokio::test]
    async fn list_package_changes_rejects_query_param_role_spoof() {
        let state = web::Data::new(Arc::new(EntitlementsState::new()));
        // `?viewer_role=admin` with no tenant context must
        // authorize nothing: the tenant guard rejects first.
        let mut params = PaginationParams::default();
        params
            .filters
            .insert("viewer_role".to_string(), "admin".to_string());
        let resp = list_package_changes(
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
    async fn list_package_changes_accepts_a_verified_caller() {
        let company_id = Uuid::now_v7().to_string();
        let state = web::Data::new(Arc::new(EntitlementsState::new()));
        let resp = list_package_changes(
            tenant_request(&company_id),
            test_ctx(&[PERM_ENTITLEMENTS_READ]),
            state,
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
