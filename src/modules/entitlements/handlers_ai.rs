// Handlers: AI (suggestion only, never purchases anything).

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::json;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;

use super::*;

// Handlers: AI (suggestion only - never purchases anything).

pub(crate) async fn ai_suggest(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    body: web::Json<AiSuggestRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let body = body.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let role = unified_core::caller::caller_from(&req)
            .roles
            .into_iter()
            .next();
        require_capability(&ctx, role.as_deref(), PERM_ENTITLEMENTS_READ)?;
        let scope = TenantScope::resolve(&req, body.company_id.clone())?;
        let limit = body.limit.unwrap_or(MAX_SUGGESTIONS).max(1).min(MAX_SUGGESTIONS);
        let denied = state.top_denied_modules(&scope.company_id, limit);
        let mut suggestions: Vec<PackageSuggestion> = Vec::new();
        for (module_id, denied_attempts) in &denied {
            suggestions.push(PackageSuggestion {
                module_id: module_id.clone(),
                denied_attempts: *denied_attempts,
                upgrade_path: state.upgrade_path(&scope.company_id, module_id),
                note: format!(
                    "The tenant tried to enable \"{module_id}\" {denied_attempts} time(s) in the last {DENIED_LOOKBACK_DAYS} days without an entitlement."
                ),
            });
        }
        // Let the AI module ground or enrich the suggestion. When
        // nothing answers, the suggestion stands on the denied
        // log alone and `ai_grounded` is false - the response says
        // so instead of pretending an AI weighed in.
        let denied_modules: Vec<Value> = denied
            .iter()
            .map(|(module_id, denied_attempts)| {
                json!({ "module_id": module_id, "denied_attempts": denied_attempts })
            })
            .collect();
        let outcome = state.emit_with_results(
            EVENT_AI_SUGGEST_REQUESTED,
            json!({
                "company_id": scope.company_id,
                "tenant_id": scope.tenant_id,
                "denied_modules": denied_modules,
            }),
        );
        let mut ai_grounded = false;
        let mut ai_note: Option<String> = None;
        if let Some(outcome) = outcome {
            if let Some(note) = outcome.get("ai_note").and_then(|value| value.as_str()) {
                ai_grounded = true;
                ai_note = Some(note.to_string());
            }
            for result in &outcome.results {
                if let Some(note) = result.get("ai_note").and_then(|value| value.as_str()) {
                    ai_grounded = true;
                    ai_note = Some(note.to_string());
                }
            }
        }
        Ok(json!({
            "tenant_id": scope.tenant_id,
            "company_id": scope.company_id,
            "suggestions": suggestions,
            "ai_grounded": ai_grounded,
            "ai_note": ai_note,
            "purchases_nothing": true,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn ai_capabilities(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    _state: web::Data<Arc<EntitlementsState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
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
        Ok(json!({
            "capabilities": [{
                "id": "suggest_requested",
                "module_id": "entitlements",
                "name": "Suggest package from denied attempts",
                "description": "Suggest which package unlocks the modules a tenant has tried to use most, from the denied-attempt audit log. A suggestion only: it never purchases or provisions anything.",
                "input_fields": ["company_id"],
                "allows_pii": false,
                "use_case": "entitlements.ai.suggest_requested",
            }],
        }))
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

    #[tokio::test]
    async fn ai_capabilities_rejects_query_param_role_spoof() {
        let state = web::Data::new(Arc::new(EntitlementsState::new()));
        // `?viewer_role=admin` with no tenant context must
        // authorize nothing: the tenant guard rejects first.
        let req = TestRequest::default()
            .uri("/api/v1/entitlements/ai/capabilities?viewer_role=admin")
            .to_http_request();
        let resp = ai_capabilities(req, test_ctx(&[PERM_ENTITLEMENTS_READ]), state).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn ai_capabilities_accepts_a_verified_caller() {
        let state = web::Data::new(Arc::new(EntitlementsState::new()));
        let req = verified_request("tester", "admin");
        let resp = ai_capabilities(req, test_ctx(&[PERM_ENTITLEMENTS_READ]), state).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
