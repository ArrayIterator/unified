// saas module -- In-memory state and small access helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use actix_web::HttpRequest;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use super::*;

pub struct SaasState {
    pub(crate) sites: RwLock<HashMap<String, Site>>,
    pub(crate) hooks: RwLock<Option<Arc<HookBus>>>,
    pub(crate) debug: AtomicBool,
}

impl SaasState {
    pub(crate) fn new() -> Self {
        Self {
            sites: RwLock::new(HashMap::new()),
            hooks: RwLock::new(None),
            debug: AtomicBool::new(false),
        }
    }

    pub(crate) fn set_hooks(&self, hooks: Arc<HookBus>) {
        *self.hooks.write().expect("saas hooks lock poisoned") = Some(hooks);
    }

    pub(crate) fn hooks(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("saas hooks lock poisoned").clone()
    }

    pub(crate) fn is_debug(&self) -> bool {
        self.debug.load(Ordering::SeqCst)
    }

    pub(crate) fn get_site(&self, id: &str) -> Option<Site> {
        self.sites
            .read()
            .expect("saas lock poisoned")
            .get(id)
            .cloned()
    }

    pub(crate) fn domain_taken(&self, domain: &str, except_id: Option<&str>) -> bool {
        self.sites
            .read()
            .expect("saas lock poisoned")
            .values()
            .any(|site| site.domain == domain && Some(site.id.as_str()) != except_id)
    }

    /// Ask the Company module whether a company exists. Returns
    /// `Ok(true)` / `Ok(false)` when it answers, `Ok(None)` when no
    /// handler is registered for `company.exists`.
    pub(crate) fn company_exists(&self, company_id: &str) -> Option<bool> {
        let hooks = self.hooks()?;
        let answer = hooks.emit(EVENT_COMPANY_EXISTS, json!({ "company_id": company_id }));
        answer.results.first().and_then(|result| {
            result
                .get("exists")
                .and_then(|v| v.as_bool())
                .or_else(|| result.get("found").and_then(|v| v.as_bool()))
        })
    }

    /// Ask the DNS verifier to check a domain. Returns `Ok(true)` /
    /// `Ok(false)` when it answers, `Ok(None)` when no handler is
    /// registered for `dns.verify`. The optional message explains a
    /// negative answer.
    pub(crate) fn dns_verify(&self, domain: &str) -> (Option<bool>, Option<String>) {
        let Some(hooks) = self.hooks() else {
            return (None, None);
        };
        let answer = hooks.emit(EVENT_DNS_VERIFY, json!({ "domain": domain }));
        match answer.results.first() {
            Some(result) => {
                let valid = result
                    .get("valid")
                    .and_then(|v| v.as_bool())
                    .or_else(|| result.get("verified").and_then(|v| v.as_bool()));
                let message = result
                    .get("message")
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_string());
                (valid, message)
            }
            None => (None, None),
        }
    }

    /// Emit an event on the hook bus and return the final context so
    /// callers can inspect the handlers' results (for example, whether
    /// a `theme.activate_requested` handler reported success). When
    /// no bus is attached, returns a fresh context carrying the
    /// payload and no results, so callers need no special case.
    pub(crate) fn emit(&self, event: &str, data: Value) -> HookContext {
        match self.hooks() {
            Some(hooks) => hooks.emit(event, data),
            None => HookContext::new(event, data),
        }
    }

    /// Insert a site, refusing when another site already uses the
    /// domain. The uniqueness check and the insert run under a single
    /// write lock, so two concurrent creates with the same domain
    /// cannot both pass the check and both insert.
    pub(crate) fn insert_site_checked(&self, site: Site) -> UnifiedResult<Site> {
        let mut sites = self.sites.write().expect("saas lock poisoned");
        if sites.values().any(|other| other.domain == site.domain) {
            return Err(UnifiedError::conflict(format!(
                "A site with domain '{}' already exists",
                site.domain
            )));
        }
        sites.insert(site.id.clone(), site.clone());
        Ok(site)
    }
}

// ---------------------------------------------------------------------------
// Caller authentication, mirroring the bi and supervision modules:
// identity comes from the server-verified caller, and the
// roles module answers `role.check` on the hook bus. Fails closed:
// no answer means not allowed.
// ---------------------------------------------------------------------------

/// Caller identity: the server-verified caller.
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

pub(crate) fn require_permission(
    ctx: &AppContext,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
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
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use actix_web::test::TestRequest;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn test_site(id: &str, domain: &str) -> Site {
        Site {
            id: id.to_string(),
            company_id: "company-1".to_string(),
            name: "Test Site".to_string(),
            domain: domain.to_string(),
            theme_id: "theme-1".to_string(),
            status: "active".to_string(),
            domain_status: "unchecked".to_string(),
            ssl_status: "pending".to_string(),
            settings: HashMap::new(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn new_state_is_not_debug() {
        assert!(!SaasState::new().is_debug());
    }

    #[test]
    fn unknown_site_is_missing() {
        assert!(SaasState::new().get_site("no-such-site").is_none());
    }

    #[test]
    fn domain_is_free_on_empty_state() {
        assert!(!SaasState::new().domain_taken("example.com", None));
    }

    #[test]
    fn caller_accepts_a_verified_caller() {
        let req = verified_request("user-1", "admin");
        let found = caller(&req).expect("caller parses headers");
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
    }

    #[test]
    fn caller_rejects_a_request_without_a_verified_caller() {
        let req = TestRequest::default().to_http_request();
        let error = caller(&req).expect_err("a request without a verified caller must fail");
        assert_eq!(error.code, 401);
    }

    #[test]
    fn caller_without_a_resolved_role_is_anonymous() {
        // A verified caller with no role resolved holds the anonymous
        // role: the identity resolves, the capability check is what
        // refuses it.
        let req = verified_request_without_roles("user-1");
        let found = caller(&req).expect("a verified caller resolves without a role");
        assert_eq!(found.role, "anonymous");
    }

    #[test]
    fn require_permission_fails_closed_without_a_role_check_handler() {
        // No `role.check` handler is registered on this bus, so the
        // preset `"allowed": false` must stand and the call is refused.
        let ctx = test_context();
        assert!(!check_capability(&ctx, "admin", PERM_SAAS_MANAGE));
        let error = require_permission(&ctx, "admin", PERM_SAAS_MANAGE)
            .expect_err("no role.check handler must deny");
        assert_eq!(error.code, 403);
    }

    #[test]
    fn require_permission_accepts_when_role_check_allows() {
        let ctx = test_context();
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                context.set("allowed", serde_json::Value::Bool(true));
            }),
        );
        assert!(require_permission(&ctx, "admin", PERM_SAAS_VIEW).is_ok());
    }

    #[test]
    fn emit_returns_the_hook_context_with_handler_results() {
        let state = SaasState::new();
        let hooks = Arc::new(HookBus::new());
        hooks.register(
            "test.event",
            "test",
            Arc::new(|context: &mut HookContext| {
                context.add_result(json!({ "answered": true }));
            }),
        );
        state.set_hooks(hooks);
        let answer = state.emit("test.event", json!({ "ping": true }));
        assert_eq!(answer.event, "test.event");
        assert_eq!(answer.results.len(), 1);
        assert_eq!(
            answer.results[0].get("answered").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn emit_without_a_bus_returns_an_empty_context() {
        let answer = SaasState::new().emit("test.event", json!({ "ping": true }));
        assert_eq!(answer.event, "test.event");
        assert!(answer.results.is_empty());
    }

    #[test]
    fn insert_site_checked_stores_a_free_domain() {
        let state = SaasState::new();
        let site = state
            .insert_site_checked(test_site("site-1", "example.com"))
            .expect("free domain inserts");
        assert_eq!(site.domain, "example.com");
        assert!(state.domain_taken("example.com", None));
    }

    #[test]
    fn insert_site_checked_rejects_a_taken_domain() {
        let state = SaasState::new();
        state
            .insert_site_checked(test_site("site-1", "example.com"))
            .expect("first insert succeeds");
        let error = state
            .insert_site_checked(test_site("site-2", "example.com"))
            .expect_err("second insert with the same domain must fail");
        assert_eq!(error.code, 409);
        assert!(state.get_site("site-2").is_none());
    }

    #[test]
    fn concurrent_inserts_with_the_same_domain_insert_only_once() {
        let state = Arc::new(SaasState::new());
        let mut handles = Vec::new();
        for index in 0..8 {
            let shared = Arc::clone(&state);
            handles.push(std::thread::spawn(move || {
                shared.insert_site_checked(test_site(&format!("site-{index}"), "race.example.com"))
            }));
        }
        let mut inserted = 0;
        for handle in handles {
            if handle.join().expect("worker finishes").is_ok() {
                inserted += 1;
            }
        }
        assert_eq!(inserted, 1, "exactly one concurrent insert must win");
        assert_eq!(
            state
                .sites
                .read()
                .expect("saas lock poisoned")
                .values()
                .filter(|site| site.domain == "race.example.com")
                .count(),
            1
        );
    }
}
