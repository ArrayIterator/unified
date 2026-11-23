// Public link handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::sharing::{
    self,
    access_attempt,
    check_allow_public_link,
    generate_link_token,
    hash_link_token,
    record_failed_link_validation,
    record_successful_link_validation,
    sharing_events,
    validate_public_link,
    PublicLink,
    ShareScope,
};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Public link handlers
// ---------------------------------------------------------------------------

pub(crate) async fn create_link(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    body: web::Json<CreateLinkRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let result: UnifiedResult<PublicLinkCreated> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SHARING_CREATE)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let settings = state.settings();
        if !settings.public_links_enabled {
            return Err(UnifiedError::forbidden(
                "Public links are disabled by configuration",
            ));
        }
        let input = body.into_inner();
        if input.expires_in_hours < 1 || input.expires_in_hours > settings.max_link_ttl_hours {
            return Err(UnifiedError::bad_request(format!(
                "expires_in_hours must be between 1 and {}",
                settings.max_link_ttl_hours
            )));
        }
        if let Some(label) = &input.label {
            require_text(label, "label", MAX_LABEL_LEN)?;
        }
        // The kind and view must be registered by the owning module;
        // a typo or an unadopted kind can never mint a link.
        if !state.registry.is_known_kind(&input.scope_kind) {
            return Err(UnifiedError::bad_request(format!(
                "scope_kind \"{}\" is not a registered shareable kind",
                input.scope_kind
            )));
        }
        if !state.registry.is_known_view(&input.scope_kind, &input.scope_view) {
            return Err(UnifiedError::bad_request(format!(
                "scope_view \"{}\" is not a registered view of kind \"{}\"",
                input.scope_view, input.scope_kind
            )));
        }
        let scope = ShareScope::new(&input.scope_kind, input.scope_id, &input.scope_view)?;
        // SERVER-SIDE enforcement of the target's allow flag: the
        // owning module answers `sharing.allow_check` from the
        // record's own `allow_public_link` flag. A false answer is
        // a 403 — never just hidden in the UI.
        if !check_allow_public_link(&ctx.hooks, &scope, &caller.actor) {
            state.log_auditor_action(
                &caller.actor,
                None,
                &format!("sharing:{}", scope.to_scope_string()),
                "sharing.link.create_denied",
                false,
            );
            return Err(UnifiedError::forbidden(
                "This record does not allow public links (allow_public_link is false or unset)",
            )
            .with_metadata("scope", serde_json::json!(scope.to_scope_string())));
        }
        let now = Utc::now();
        let expires_at = now + chrono::Duration::hours(input.expires_in_hours);
        let (token, token_hash) = generate_link_token();
        let link = PublicLink {
            id: Uuid::now_v7(),
            token_hash: token_hash.clone(),
            scope: scope.clone(),
            created_by: caller.actor.clone(),
            expires_at,
            revoked_at: None,
            created_at: now,
        };
        state.links_by_hash.insert(token_hash, link.id);
        state.links.insert(link.id, link.clone());
        state.log_auditor_action(
            &caller.actor,
            None,
            &format!("sharing:{}", scope.to_scope_string()),
            "sharing.link.created",
            true,
        );
        state.emit(
            sharing_events::LINK_CREATED,
            json!({
                "link_id": link.id.to_string(),
                "scope_kind": scope.kind,
                "scope_id": scope.id.to_string(),
                "scope_view": scope.view,
                "created_by": caller.actor,
                "expires_at": expires_at.to_rfc3339(),
            }),
        );
        // HMAC-signed payload for the link, when a secret is
        // configured. A missing secret means no signed payload —
        // the link still works through the token.
        let secret_key = ctx
            .config
            .read()
            .expect("config lock poisoned")
            .app
            .secret_key
            .clone();
        let signed_payload = if secret_key.len() >= 32 {
            Some(sharing::sign_link_url(
                secret_key.as_bytes(),
                link.id,
                &scope,
                expires_at,
            ))
        } else {
            None
        };
        let url = format!(
            "/api/v1/sharing/public/{}?scope_kind={}&scope_id={}&scope_view={}",
            token, scope.kind, scope.id, scope.view
        );
        Ok(PublicLinkCreated {
            link: PublicLinkView::from(&link),
            token,
            url,
            signed_payload,
        })
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_links(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let result: UnifiedResult<PageResult<PublicLinkView>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SHARING_VIEW)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let scope = caller_company(&_tenant, &req)?;
        let params = query.into_inner();
        let now = Utc::now();
        let kind_filter = params.filters.get("scope_kind").cloned();
        let created_by_filter = params.filters.get("created_by").cloned();
        let revoked_filter = params.filters.get("revoked").cloned();
        let expired_filter = params.filters.get("expired").cloned();
        let mut items: Vec<PublicLinkView> = state
            .links
            .iter()
            .map(|entry| entry.value().clone())
            // Company scope first: links carry no company of
            // their own, so the creator is the company proxy — a
            // scoped caller sees only links they created.
            .filter(|link| link_in_scope(link, &scope, &caller.actor))
            .filter(|link| match kind_filter.as_deref() {
                Some(wanted) => link.scope.kind == wanted,
                None => true,
            })
            .filter(|link| match created_by_filter.as_deref() {
                Some(wanted) => link.created_by == wanted,
                None => true,
            })
            .filter(|link| match revoked_filter.as_deref() {
                Some("true") => link.is_revoked(),
                Some("false") => !link.is_revoked(),
                _ => true,
            })
            .filter(|link| match expired_filter.as_deref() {
                Some("true") => link.is_expired(now),
                Some("false") => !link.is_expired(now),
                _ => true,
            })
            .map(|link| PublicLinkView::from(&link))
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_link(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let result: UnifiedResult<PublicLinkView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SHARING_VIEW)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let scope = caller_company(&_tenant, &req)?;
        let link = load_link(&state, &path.into_inner())?;
        require_link_in_scope(&link, &scope, &caller.actor)?;
        Ok(PublicLinkView::from(&link))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn revoke_link(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RevokeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let result: UnifiedResult<PublicLinkView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SHARING_REVOKE)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let input = body.into_inner();
        let reason = match &input.reason {
            Some(value) => Some(require_text(value, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        let id = parse_uuid(&path.into_inner(), "public link")?;
        // Scope check before the write guard below: the link
        // must be in the caller's company scope to revoke it.
        let link = load_link(&state, &id.to_string())?;
        require_link_in_scope(&link, &scope, &caller.actor)?;
        let now = Utc::now();
        // Scope the write guard: it must be released before
        // load_link below re-locks the same shard. Never shadow
        // the guard with a clone and drop the clone: that drops
        // the owned copy, not the guard.
        {
            let mut link = state
                .links
                .get_mut(&id)
                .ok_or_else(|| UnifiedError::not_found(format!("Public link not found: {id}")))?;
            if link.is_revoked() {
                return Err(UnifiedError::conflict("Public link is already revoked"));
            }
            link.revoked_at = Some(now);
        }
        state.log_auditor_action(
            &caller.actor,
            None,
            &format!("sharing:{}", id),
            "sharing.link.revoked",
            true,
        );
        state.emit(
            sharing_events::LINK_REVOKED,
            json!({
                "link_id": id.to_string(),
                "revoked_by": caller.actor,
                "reason": reason,
            }),
        );
        let link = load_link(&state, &id.to_string())?;
        Ok(PublicLinkView::from(&link))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_link_access_log(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let result: UnifiedResult<PageResult<PublicLinkAccessLogEntry>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SHARING_VIEW)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let scope = caller_company(&_tenant, &req)?;
        let params = query.into_inner();
        let link = load_link(&state, &path.into_inner())?;
        require_link_in_scope(&link, &scope, &caller.actor)?;
        let allowed_filter = params.filters.get("allowed").cloned();
        let reason_filter = params.filters.get("reason").cloned();
        let mut items: Vec<PublicLinkAccessLogEntry> = state
            .access_log
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.link_id == Some(link.id))
            .filter(|entry| match allowed_filter.as_deref() {
                Some("true") => entry.allowed,
                Some("false") => !entry.allowed,
                _ => true,
            })
            .filter(|entry| match reason_filter.as_deref() {
                Some(wanted) => entry.reason == wanted,
                None => true,
            })
            .collect();
        items.sort_by(|a, b| b.accessed_at.cmp(&a.accessed_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_access_log(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let result: UnifiedResult<PageResult<PublicLinkAccessLogEntry>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SHARING_VIEW)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let scope = caller_company(&_tenant, &req)?;
        let params = query.into_inner();
        let allowed_filter = params.filters.get("allowed").cloned();
        let link_filter = params.filters.get("link_id").cloned();
        let reason_filter = params.filters.get("reason").cloned();
        let mut items: Vec<PublicLinkAccessLogEntry> = state
            .access_log
            .iter()
            .map(|entry| entry.value().clone())
            // Entries are scoped through their parent link: a
            // scoped caller sees only entries for links they
            // created. Entries with no link are platform-wide
            // and visible to unresolved callers only.
            .filter(|entry| match entry.link_id {
                Some(link_id) => state
                    .links
                    .get(&link_id)
                    .map(|link| link_in_scope(link.value(), &scope, &caller.actor))
                    .unwrap_or(false),
                None => scope.is_none(),
            })
            .filter(|entry| match allowed_filter.as_deref() {
                Some("true") => entry.allowed,
                Some("false") => !entry.allowed,
                _ => true,
            })
            .filter(|entry| match link_filter.as_deref() {
                Some(wanted) => entry
                    .link_id
                    .is_some_and(|id| id.to_string() == wanted),
                None => true,
            })
            .filter(|entry| match reason_filter.as_deref() {
                Some(wanted) => entry.reason == wanted,
                None => true,
            })
            .collect();
        items.sort_by(|a, b| b.accessed_at.cmp(&a.accessed_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_shareable_kinds(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let result: UnifiedResult<Vec<sharing::ShareableKind>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SHARING_VIEW)?;
        reject_auditor_caller(&state, &caller.actor)?;
        Ok(state.registry.kinds())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// PUBLIC route: validate a link token and return its scope
/// descriptor. No identity headers, no capability check — the link
/// token IS the credential, and it grants read-only access to
/// exactly one scoped resource. Serves NO resource content: the
/// owning module serves content after validating the link through
/// `core::sharing::validate_public_link` itself. Gated by the API
/// Manager `ApiAccessRule` for this path (default deny — see the
/// module header). Every attempt is written to the access log and
/// a failed attempt feeds the hardening lockout.
pub(crate) async fn get_public_by_token(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PublicLinkQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let now = Utc::now();
    let ip = client_ip(&req);
    let agent = user_agent(&req);
    let result: UnifiedResult<Value> = (|| {
        let presented = path.into_inner();
        if presented.trim().is_empty() {
            return Err(UnifiedError::unauthorized("Link token is required"));
        }
        let query = query.into_inner();
        let scope_kind = query.scope_kind.ok_or_else(|| {
            UnifiedError::bad_request("scope_kind query parameter is required")
        })?;
        let scope_id_raw = query.scope_id.ok_or_else(|| {
            UnifiedError::bad_request("scope_id query parameter is required")
        })?;
        let scope_view = query.scope_view.ok_or_else(|| {
            UnifiedError::bad_request("scope_view query parameter is required")
        })?;
        let scope_id = parse_uuid(&scope_id_raw, "scope")?;
        let requested = ShareScope::new(&scope_kind, scope_id, &scope_view)?;
        // Hash lookup first (the raw token is never stored), then
        // the shared validator decides. The client-visible error is
        // always the same generic text, whatever the denial reason.
        let token_hash = hash_link_token(&presented);
        let link_id = state.links_by_hash.get(&token_hash).map(|entry| *entry.value());
        let link = link_id.and_then(|id| state.links.get(&id).map(|entry| entry.value().clone()));
        let outcome = validate_public_link(link.as_ref(), &presented, &requested, now);
        // Every attempt is logged — allowed AND denied — with only
        // hashes of the IP and user agent.
        let attempt = access_attempt(link_id, ip.as_deref(), agent.as_deref(), &outcome, now);
        let attempt_entry = PublicLinkAccessLogEntry {
            id: attempt.id,
            link_id: attempt.link_id,
            accessed_at: attempt.accessed_at,
            ip_hash: attempt.ip_hash,
            user_agent_hash: attempt.user_agent_hash,
            allowed: attempt.allowed,
            reason: attempt.reason.clone(),
        };
        state.access_log.insert(attempt_entry.id, attempt_entry);
        match &outcome {
            Ok(scope) => {
                let link = link.expect("validated link exists");
                record_successful_link_validation(
                    &unified_core::hardening::shared_state(),
                    ip.as_deref(),
                    link.id,
                );
                state.emit(
                    sharing_events::LINK_ACCESSED,
                    json!({
                        "link_id": link.id.to_string(),
                        "scope_kind": scope.kind,
                        "scope_id": scope.id.to_string(),
                        "scope_view": scope.view,
                    }),
                );
                Ok(json!({
                    "link_id": link.id,
                    "scope": scope,
                    "scope_string": scope.to_scope_string(),
                    "expires_at": link.expires_at,
                    "created_by": link.created_by,
                    "read_only": true,
                }))
            }
            Err(denial) => {
                record_failed_link_validation(
                    &unified_core::hardening::shared_state(),
                    ip.as_deref(),
                    link_id,
                );
                state.emit(
                    sharing_events::LINK_ACCESS_DENIED,
                    json!({
                        "link_id": link_id.map(|id| id.to_string()),
                        "reason": denial.as_str(),
                    }),
                );
                let _ = denial;
                Err(UnifiedError::unauthorized(
                    "This public link is not valid for the requested resource",
                ))
            }
        }
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {

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
    use chrono::Utc;

    /// Context plus state sharing one hook bus whose `role.check`
    /// answer allows every permission, so the handlers run end to
    /// end.
    fn test_env() -> (web::Data<Arc<AppContext>>, web::Data<AuditorsState>) {
        let bus = Arc::new(HookBus::new());
        bus.register(
            "role.check",
            "test",
            Arc::new(|ctx: &mut HookContext| {
                ctx.set("allowed", serde_json::json!(true));
            }),
        );
        let ctx = AppContext::new(
            AppConfig::default(),
            Arc::clone(&bus),
            Arc::new(ModuleRegistry::new()),
        );
        let state = AuditorsState::default();
        *state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(bus);
        (web::Data::new(Arc::new(ctx)), web::Data::new(state))
    }

    /// The company the handler tests act as (C3 task 315).
    const TEST_COMPANY_ID: &str = "22222222-3333-4444-8555-666666666666";

    /// A request scoped to the test company (C3 task 315):
    /// mutation handlers require a resolved company, so the
    /// happy-path tests carry one on both the tenant context and
    /// the verified caller.
    fn admin_request() -> HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid("admin-1"), Vec::new())
                .with_company(TEST_COMPANY_ID.to_string())
                .with_roles(vec!["admin".to_string()]),
        );
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: TEST_COMPANY_ID.to_string(),
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

    fn seed_link(state: &AuditorsState) -> PublicLink {
        let now = Utc::now();
        let link = PublicLink {
            id: Uuid::now_v7(),
            token_hash: "deadbeef".to_string(),
            scope: ShareScope::new(
                "network_tools_run",
                Uuid::now_v7(),
                "summary",
            )
            .expect("valid scope"),
            created_by: test_user_uuid("admin-1").to_string(),
            expires_at: now + chrono::Duration::hours(24),
            revoked_at: None,
            created_at: now,
        };
        state.links.insert(link.id, link.clone());
        link
    }

    #[tokio::test]
    async fn revoke_link_completes_and_revokes_link() {
        // Regression test for the shadowed-clone deadlock: the old
        // code held the links write guard while load_link re-locked
        // the same shard, so this call hung forever.
        let (ctx, state) = test_env();
        let link = seed_link(&state);
        let response = revoke_link(
            ctx,
            state.clone(),
            admin_request(),
            web::Path::from(link.id.to_string()),
            web::Json(RevokeRequest {
                reason: Some("no longer shared".to_string()),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let stored = state
            .links
            .get(&link.id)
            .expect("link kept")
            .value()
            .clone();
        assert!(stored.is_revoked());
        assert!(stored.revoked_at.is_some());
    }

    #[tokio::test]
    async fn revoke_link_twice_conflicts() {
        let (ctx, state) = test_env();
        let link = seed_link(&state);
        let first = revoke_link(
            ctx.clone(),
            state.clone(),
            admin_request(),
            web::Path::from(link.id.to_string()),
            web::Json(RevokeRequest { reason: None }),
        )
        .await;
        assert_eq!(first.status(), StatusCode::OK);
        let second = revoke_link(
            ctx,
            state,
            admin_request(),
            web::Path::from(link.id.to_string()),
            web::Json(RevokeRequest { reason: None }),
        )
        .await;
        assert_eq!(second.status(), StatusCode::CONFLICT);
    }
}
