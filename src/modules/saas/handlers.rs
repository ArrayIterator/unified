// saas module -- HTTP handlers.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::hooks::{HookContext, events};
use unified_core::json as api_json;
use unified_core::modules::Module;
use unified_core::pagination::{PageResult, PaginationParams};
use crate::themes::engine::THEME_ACTIVATED_EVENT;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use super::*;

pub(crate) fn not_found_site(debug: bool) -> HttpResponse {
    api_json::error_response(&UnifiedError::not_found("Site not found"), debug)
}

/// Authenticate the caller from the `X-User-Id` / `X-Role` headers
/// and require `permission`. Every handler calls this first; a
/// missing identity or a missing permission fails the request before
/// any site is read or changed.
fn authorize(ctx: &AppContext, req: &HttpRequest, permission: &str) -> Result<(), UnifiedError> {
    let caller = caller(req)?;
    require_permission(ctx, &caller.role, permission)
}

/// True when the `theme.activate_requested` answer reports a
/// successful activation. The theme module answers with a result
/// object `{"activated": bool, "theme_id": ...}`; any result with
/// `activated` true counts, so extra contributors cannot hide a
/// success behind a failure (or the reverse).
fn theme_activation_succeeded(answer: &HookContext) -> bool {
    answer
        .results
        .iter()
        .any(|result| result.get("activated").and_then(Value::as_bool).unwrap_or(false))
}

/// Forward a successful theme activation as `theme.activated` so the
/// rendering engines (dashboards, frontend) switch to the new theme.
/// The theme module itself cannot emit it: its synchronous hook
/// handler only receives a `HookContext`, never the bus. A failed
/// activation emits nothing, so the previously active theme keeps
/// rendering. The payload matches the admin activation path
/// (`theme_id` plus `active: true`); `site_id` names the site that
/// triggered the activation.
fn emit_theme_activated(state: &SaasState, answer: HookContext, site: &Site) {
    if theme_activation_succeeded(&answer) {
        state.emit(
            THEME_ACTIVATED_EVENT,
            json!({
                "theme_id": site.theme_id,
                "site_id": site.id,
                "active": true,
            }),
        );
    }
}

// HTTP handlers

pub(crate) async fn list_sites(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SaasState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = state.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_SAAS_VIEW) {
        return api_json::error_response(&error, debug);
    }
    let pagination = query.normalized();
    let mut sites: Vec<Site> = state
        .sites
        .read()
        .expect("saas lock poisoned")
        .values()
        .cloned()
        .collect();
    if let Some(company_id) = pagination.filters.get("company_id") {
        sites.retain(|site| site.company_id == *company_id);
    }
    if let Some(status) = pagination.filters.get("status") {
        sites.retain(|site| site.status == *status);
    }
    if let Some(domain_status) = pagination.filters.get("domain_status") {
        sites.retain(|site| site.domain_status == *domain_status);
    }
    if let Some(search) = &pagination.search {
        let needle = search.to_lowercase();
        sites.retain(|site| {
            site.name.to_lowercase().contains(&needle)
                || site.domain.to_lowercase().contains(&needle)
        });
    }
    let sort = query.allowed_sort(&["name", "domain", "status", "created_at", "updated_at"], "created_at");
    match sort.as_str() {
        "name" => sites.sort_by(|a, b| a.name.cmp(&b.name)),
        "domain" => sites.sort_by(|a, b| a.domain.cmp(&b.domain)),
        "status" => sites.sort_by(|a, b| a.status.cmp(&b.status)),
        "updated_at" => sites.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
        _ => sites.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if pagination.order == "desc" {
        sites.reverse();
    }
    let total = sites.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<Site> = sites
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}


pub(crate) async fn create_site(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SaasState>>,
    req: HttpRequest,
    body: web::Json<SiteInput>,
) -> HttpResponse {
    let state: Arc<SaasState> = state.get_ref().clone();
    let debug = state.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_SAAS_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let input = body.into_inner();
    if input.company_id.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("company_id must not be empty"),
            debug,
        );
    }
    if input.name.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("Site name must not be empty"),
            debug,
        );
    }
    if input.theme_id.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("theme_id must not be empty"),
            debug,
        );
    }
    let domain = input.domain.trim().to_lowercase();
    if let Err(error) = validate_domain(&domain) {
        return api_json::error_response(&error, debug);
    }
    if state.domain_taken(&domain, None) {
        return api_json::error_response(
            &UnifiedError::conflict(format!("A site with domain '{domain}' already exists")),
            debug,
        );
    }

    let mut warnings: Vec<String> = Vec::new();

    // 1. Company check through the hook bus.
    match state.company_exists(input.company_id.trim()) {
        Some(true) => {}
        Some(false) => {
            return api_json::error_response(
                &UnifiedError::unprocessable(format!(
                    "Company '{}' does not exist",
                    input.company_id.trim()
                )),
                debug,
            );
        }
        None => warnings.push(
            "Company could not be verified: no company handler answered the company.exists event; accepted without verification"
                .to_string(),
        ),
    }

    // 2. Domain check through the DNS verifier.
    let domain_status = match state.dns_verify(&domain) {
        (Some(true), _) => "verified".to_string(),
        (Some(false), message) => {
            let detail = message
                .map(|message| format!(": {message}"))
                .unwrap_or_default();
            return api_json::error_response(
                &UnifiedError::unprocessable(format!(
                    "Domain '{domain}' failed DNS verification{detail}"
                )),
                debug,
            );
        }
        (None, _) => {
            warnings.push(
                "Domain could not be verified: no DNS verifier answered the dns.verify event; accepted unchecked"
                    .to_string(),
            );
            "unchecked".to_string()
        }
    };

    // 3. Create the site. The insert re-checks domain uniqueness
    // under the same write lock that performs the insert, so the
    // read-lock check above is only a fast path: two concurrent
    // creates with the same domain cannot both insert.
    let now = Utc::now().to_rfc3339();
    let site = Site {
        id: Uuid::now_v7().to_string(),
        company_id: input.company_id.trim().to_string(),
        name: input.name.trim().to_string(),
        domain: domain.clone(),
        theme_id: input.theme_id.trim().to_string(),
        status: "active".to_string(),
        domain_status,
        // SSL renewal is requested through the ssl.renew event below; until
        // the SSL module reports back, the status is pending.
        ssl_status: "pending".to_string(),
        settings: HashMap::new(),
        created_at: now.clone(),
        updated_at: now,
    };
    let site = match state.insert_site_checked(site) {
        Ok(site) => site,
        Err(error) => return api_json::error_response(&error, debug),
    };

    // 4. Wire the ERP chain through hooks: provisioned, SSL, theme.
    state.emit(
        EVENT_SITE_PROVISIONED,
        json!({
            "site_id": site.id,
            "company_id": site.company_id,
            "domain": site.domain,
            "theme_id": site.theme_id
        }),
    );
    state.emit(
        events::SSL_RENEW,
        json!({ "domain": site.domain, "site_id": site.id }),
    );
    // The theme module activates the theme synchronously and reports
    // `{"activated": bool}`; on success forward it as
    // `theme.activated` so the rendering engines switch themes.
    let activation = state.emit(
        EVENT_THEME_ACTIVATE_REQUESTED,
        json!({ "theme_id": site.theme_id, "site_id": site.id }),
    );
    emit_theme_activated(&state, activation, &site);

    let mut data = serde_json::to_value(&site).expect("site serializes");
    if !warnings.is_empty() {
        data.as_object_mut()
            .expect("site value is an object")
            .insert("warnings".to_string(), json!(warnings));
    }
    api_json::success_response(data, 201)
}


pub(crate) async fn get_site(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SaasState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_SAAS_VIEW) {
        return api_json::error_response(&error, debug);
    }
    match state.get_site(&path.into_inner()) {
        Some(site) => api_json::success_response(site, 200),
        None => not_found_site(debug),
    }
}


pub(crate) async fn update_site(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SaasState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SiteUpdateInput>,
) -> HttpResponse {
    let state: Arc<SaasState> = state.get_ref().clone();
    let debug = state.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_SAAS_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let id = path.into_inner();
    let input = body.into_inner();
    // A provided company_id, name, or theme_id must not be blank,
    // exactly as creation requires.
    if let Err(error) = validate_site_update(&input) {
        return api_json::error_response(&error, debug);
    }
    if let Some(domain) = &input.domain {
        let domain = domain.trim().to_lowercase();
        if let Err(error) = validate_domain(&domain) {
            return api_json::error_response(&error, debug);
        }
        if state.domain_taken(&domain, Some(&id)) {
            return api_json::error_response(
                &UnifiedError::conflict(format!("A site with domain '{domain}' already exists")),
                debug,
            );
        }
    }
    let mut sites = state.sites.write().expect("saas lock poisoned");
    // Re-check domain uniqueness under the write lock: the read-lock
    // check above is only a fast path, and the check-and-update must
    // be atomic for concurrent updates.
    if let Some(domain) = &input.domain {
        let domain = domain.trim().to_lowercase();
        if sites
            .values()
            .any(|other| other.domain == domain && other.id != id)
        {
            return api_json::error_response(
                &UnifiedError::conflict(format!("A site with domain '{domain}' already exists")),
                debug,
            );
        }
    }
    let site = match sites.get_mut(&id) {
        Some(site) => site,
        None => return not_found_site(debug),
    };
    let mut domain_changed = false;
    let mut theme_changed = false;
    if let Some(company_id) = input.company_id {
        site.company_id = company_id.trim().to_string();
    }
    if let Some(name) = input.name {
        site.name = name.trim().to_string();
    }
    if let Some(domain) = input.domain {
        let new_domain = domain.trim().to_lowercase();
        if new_domain != site.domain {
            domain_changed = true;
        }
        site.domain = new_domain;
        // A changed domain must be re-verified on the next provisioning
        // step; mark it unchecked until the DNS verifier confirms it.
        site.domain_status = "unchecked".to_string();
        site.ssl_status = "pending".to_string();
    }
    if let Some(theme_id) = input.theme_id {
        let new_theme = theme_id.trim().to_string();
        if new_theme != site.theme_id {
            theme_changed = true;
        }
        site.theme_id = new_theme;
    }
    site.updated_at = Utc::now().to_rfc3339();
    let site = site.clone();
    drop(sites);
    // A new domain needs fresh SSL provisioning and a new theme needs
    // activation, exactly as creation requests them; without these
    // events the new domain would never be re-provisioned.
    if domain_changed {
        state.emit(
            events::SSL_RENEW,
            json!({ "domain": site.domain, "site_id": site.id }),
        );
    }
    if theme_changed {
        // Same forwarding as creation: only a successful activation
        // becomes a `theme.activated` event.
        let activation = state.emit(
            EVENT_THEME_ACTIVATE_REQUESTED,
            json!({ "theme_id": site.theme_id, "site_id": site.id }),
        );
        emit_theme_activated(&state, activation, &site);
    }
    api_json::success_response(site, 200)
}


pub(crate) async fn delete_site(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SaasState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_SAAS_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let removed = state
        .sites
        .write()
        .expect("saas lock poisoned")
        .remove(&path.into_inner());
    match removed {
        Some(site) => {
            // Tell downstream listeners the site is gone, like
            // create / suspend / activate announce their changes.
            state.emit(
                EVENT_SITE_DELETED,
                json!({ "site_id": site.id, "domain": site.domain }),
            );
            api_json::success_response(json!({ "deleted": true, "id": site.id }), 200)
        }
        None => not_found_site(debug),
    }
}


pub(crate) async fn suspend_site(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SaasState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let state: Arc<SaasState> = state.get_ref().clone();
    let debug = state.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_SAAS_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let id = path.into_inner();
    let mut sites = state.sites.write().expect("saas lock poisoned");
    let site = match sites.get_mut(&id) {
        Some(site) => site,
        None => return not_found_site(debug),
    };
    if site.status != "active" {
        return api_json::error_response(
            &UnifiedError::conflict(format!(
                "Only an active site can be suspended (current status: {})",
                site.status
            )),
            debug,
        );
    }
    site.status = "suspended".to_string();
    site.updated_at = Utc::now().to_rfc3339();
    let site = site.clone();
    drop(sites);
    state.emit(
        EVENT_SITE_SUSPENDED,
        json!({ "site_id": site.id, "domain": site.domain }),
    );
    api_json::success_response(site, 200)
}


pub(crate) async fn activate_site(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SaasState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let state: Arc<SaasState> = state.get_ref().clone();
    let debug = state.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_SAAS_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let id = path.into_inner();
    let mut sites = state.sites.write().expect("saas lock poisoned");
    let site = match sites.get_mut(&id) {
        Some(site) => site,
        None => return not_found_site(debug),
    };
    if site.status != "suspended" {
        return api_json::error_response(
            &UnifiedError::conflict(format!(
                "Only a suspended site can be activated (current status: {})",
                site.status
            )),
            debug,
        );
    }
    site.status = "active".to_string();
    site.updated_at = Utc::now().to_rfc3339();
    let site = site.clone();
    drop(sites);
    state.emit(
        EVENT_SITE_ACTIVATED,
        json!({ "site_id": site.id, "domain": site.domain }),
    );
    api_json::success_response(site, 200)
}


pub(crate) async fn get_site_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SaasState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_SAAS_VIEW) {
        return api_json::error_response(&error, debug);
    }
    match state.get_site(&path.into_inner()) {
        Some(site) => api_json::success_response(
            json!({ "site_id": site.id, "settings": site.settings }),
            200,
        ),
        None => not_found_site(debug),
    }
}


pub(crate) async fn update_site_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SaasState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<Value>,
) -> HttpResponse {
    let debug = state.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_SAAS_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let id = path.into_inner();
    let body = body.into_inner();
    // Accept either `{ "settings": { ... } }` or a flat settings object.
    // The body is consumed by value, so keys and values move into the
    // merged map instead of being cloned out of a borrowed body.
    let incoming: HashMap<String, Value> = match body {
        Value::Object(mut map) => match map.remove("settings") {
            Some(Value::Object(settings)) => settings.into_iter().collect(),
            Some(_) => {
                return api_json::error_response(
                    &UnifiedError::bad_request("'settings' must be a JSON object"),
                    debug,
                )
            }
            None => map.into_iter().collect(),
        },
        _ => {
            return api_json::error_response(
                &UnifiedError::bad_request("Settings body must be a JSON object"),
                debug,
            )
        }
    };
    let mut sites = state.sites.write().expect("saas lock poisoned");
    let site = match sites.get_mut(&id) {
        Some(site) => site,
        None => return not_found_site(debug),
    };
    // Merge: provided keys overwrite, an explicit null deletes the key.
    for (key, value) in incoming {
        if value.is_null() {
            site.settings.remove(&key);
        } else {
            site.settings.insert(key, value);
        }
    }
    site.updated_at = Utc::now().to_rfc3339();
    let settings = site.settings.clone();
    let site_id = site.id.clone();
    api_json::success_response(json!({ "site_id": site_id, "settings": settings }), 200)
}

// Module

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
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;
    use std::sync::Mutex;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    /// A context whose `role.check` handler allows every permission.
    fn allowing_context() -> AppContext {
        let ctx = test_context();
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(|context: &mut HookContext| {
                context.set("allowed", serde_json::Value::Bool(true));
            }),
        );
        ctx
    }

    fn authed_request() -> HttpRequest {
        verified_request("user-1", "admin")
    }

    fn anonymous_request() -> HttpRequest {
        TestRequest::default().to_http_request()
    }

    /// Context + state sharing one hook bus, so events emitted by the
    /// handlers can be observed in tests.
    fn fixture() -> (web::Data<Arc<AppContext>>, web::Data<Arc<SaasState>>) {
        let ctx = allowing_context();
        let state = Arc::new(SaasState::new());
        state.set_hooks(ctx.hooks.clone());
        (web::Data::new(Arc::new(ctx)), web::Data::new(state))
    }

    /// Record every payload emitted for `event` on the fixture bus.
    fn capture(ctx: &AppContext, event: &str) -> Arc<Mutex<Vec<Value>>> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        ctx.hooks.register(
            event,
            "test",
            Arc::new(move |context: &mut HookContext| {
                sink.lock()
                    .expect("capture lock poisoned")
                    .push(context.data.clone());
            }),
        );
        seen
    }

    /// Register a fake `theme.activate_requested` handler that
    /// answers with the given `activated` flag, echoing the requested
    /// theme id back in its result like the real theme module does.
    fn answer_theme_activation(ctx: &AppContext, activated: bool) {
        ctx.hooks.register(
            EVENT_THEME_ACTIVATE_REQUESTED,
            "test",
            Arc::new(move |context: &mut HookContext| {
                let theme_id = context
                    .get("theme_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                context.add_result(json!({ "activated": activated, "theme_id": theme_id }));
            }),
        );
    }

    fn stored_site(state: &SaasState, id: &str, domain: &str) -> Site {
        let site = Site {
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
        };
        state
            .sites
            .write()
            .expect("saas lock poisoned")
            .insert(site.id.clone(), site.clone());
        site
    }

    fn create_input(domain: &str) -> web::Json<SiteInput> {
        web::Json(SiteInput {
            company_id: "company-1".to_string(),
            name: "New Site".to_string(),
            domain: domain.to_string(),
            theme_id: "theme-1".to_string(),
        })
    }

    #[tokio::test]
    async fn list_sites_rejects_anonymous_callers() {
        let (ctx, state) = fixture();
        let response = list_sites(
            ctx,
            state,
            anonymous_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_sites_allows_an_authorised_caller() {
        let (ctx, state) = fixture();
        let response = list_sites(
            ctx,
            state,
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn create_site_rejects_anonymous_callers() {
        let (ctx, state) = fixture();
        let response = create_site(
            ctx,
            state,
            anonymous_request(),
            create_input("new.example.com"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create_site_rejects_a_duplicate_domain() {
        let (ctx, state) = fixture();
        stored_site(&state, "site-1", "taken.example.com");
        let response = create_site(
            ctx,
            state,
            authed_request(),
            create_input("taken.example.com"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn update_site_rejects_a_blank_company_id() {
        let (ctx, state) = fixture();
        stored_site(&state, "site-1", "example.com");
        let body = web::Json(SiteUpdateInput {
            company_id: Some("   ".to_string()),
            name: None,
            domain: None,
            theme_id: None,
        });
        let response = update_site(
            ctx,
            state,
            authed_request(),
            web::Path::from("site-1".to_string()),
            body,
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_site_rejects_a_blank_theme_id() {
        let (ctx, state) = fixture();
        stored_site(&state, "site-1", "example.com");
        let body = web::Json(SiteUpdateInput {
            company_id: None,
            name: None,
            domain: None,
            theme_id: Some(String::new()),
        });
        let response = update_site(
            ctx,
            state,
            authed_request(),
            web::Path::from("site-1".to_string()),
            body,
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_site_emits_site_deleted() {
        let (ctx, state) = fixture();
        let deleted = capture(&ctx, EVENT_SITE_DELETED);
        stored_site(&state, "site-1", "gone.example.com");
        let response = delete_site(
            ctx,
            state,
            authed_request(),
            web::Path::from("site-1".to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let seen = deleted.lock().expect("capture lock poisoned");
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].get("site_id").and_then(Value::as_str),
            Some("site-1")
        );
        assert_eq!(
            seen[0].get("domain").and_then(Value::as_str),
            Some("gone.example.com")
        );
    }

    #[tokio::test]
    async fn update_site_reprovisions_ssl_and_theme_on_change() {
        let (ctx, state) = fixture();
        let ssl = capture(&ctx, events::SSL_RENEW);
        let theme = capture(&ctx, EVENT_THEME_ACTIVATE_REQUESTED);
        stored_site(&state, "site-1", "old.example.com");
        let body = web::Json(SiteUpdateInput {
            company_id: None,
            name: None,
            domain: Some("new.example.com".to_string()),
            theme_id: Some("theme-2".to_string()),
        });
        let response = update_site(
            ctx,
            state,
            authed_request(),
            web::Path::from("site-1".to_string()),
            body,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let ssl_seen = ssl.lock().expect("capture lock poisoned");
        assert_eq!(ssl_seen.len(), 1);
        assert_eq!(
            ssl_seen[0].get("domain").and_then(Value::as_str),
            Some("new.example.com")
        );
        let theme_seen = theme.lock().expect("capture lock poisoned");
        assert_eq!(theme_seen.len(), 1);
        assert_eq!(
            theme_seen[0].get("theme_id").and_then(Value::as_str),
            Some("theme-2")
        );
    }

    #[tokio::test]
    async fn create_site_emits_theme_activated_when_activation_succeeds() {
        let (ctx, state) = fixture();
        answer_theme_activation(&ctx, true);
        let activated = capture(&ctx, THEME_ACTIVATED_EVENT);
        let response = create_site(
            ctx,
            state,
            authed_request(),
            create_input("new.example.com"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let seen = activated.lock().expect("capture lock poisoned");
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].get("theme_id").and_then(Value::as_str),
            Some("theme-1")
        );
        assert_eq!(
            seen[0].get("active").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[tokio::test]
    async fn create_site_emits_no_theme_activated_when_activation_fails() {
        let (ctx, state) = fixture();
        answer_theme_activation(&ctx, false);
        let activated = capture(&ctx, THEME_ACTIVATED_EVENT);
        let response = create_site(
            ctx,
            state,
            authed_request(),
            create_input("new.example.com"),
        )
        .await;
        // Site creation still succeeds: a failed theme activation
        // does not fail provisioning, it just emits nothing.
        assert_eq!(response.status(), StatusCode::CREATED);
        assert!(activated.lock().expect("capture lock poisoned").is_empty());
    }

    #[tokio::test]
    async fn update_site_emits_theme_activated_when_theme_change_succeeds() {
        let (ctx, state) = fixture();
        answer_theme_activation(&ctx, true);
        let activated = capture(&ctx, THEME_ACTIVATED_EVENT);
        stored_site(&state, "site-1", "example.com");
        let body = web::Json(SiteUpdateInput {
            company_id: None,
            name: None,
            domain: None,
            theme_id: Some("theme-2".to_string()),
        });
        let response = update_site(
            ctx,
            state,
            authed_request(),
            web::Path::from("site-1".to_string()),
            body,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let seen = activated.lock().expect("capture lock poisoned");
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].get("theme_id").and_then(Value::as_str),
            Some("theme-2")
        );
        assert_eq!(
            seen[0].get("site_id").and_then(Value::as_str),
            Some("site-1")
        );
        assert_eq!(
            seen[0].get("active").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[tokio::test]
    async fn update_site_emits_no_theme_activated_when_theme_change_fails() {
        let (ctx, state) = fixture();
        answer_theme_activation(&ctx, false);
        let activated = capture(&ctx, THEME_ACTIVATED_EVENT);
        stored_site(&state, "site-1", "example.com");
        let body = web::Json(SiteUpdateInput {
            company_id: None,
            name: None,
            domain: None,
            theme_id: Some("theme-2".to_string()),
        });
        let response = update_site(
            ctx,
            state,
            authed_request(),
            web::Path::from("site-1".to_string()),
            body,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(activated.lock().expect("capture lock poisoned").is_empty());
    }

    #[tokio::test]
    async fn update_site_with_unchanged_values_emits_nothing() {
        let (ctx, state) = fixture();
        let ssl = capture(&ctx, events::SSL_RENEW);
        let theme = capture(&ctx, EVENT_THEME_ACTIVATE_REQUESTED);
        stored_site(&state, "site-1", "same.example.com");
        let body = web::Json(SiteUpdateInput {
            company_id: None,
            name: Some("Renamed".to_string()),
            domain: Some("same.example.com".to_string()),
            theme_id: Some("theme-1".to_string()),
        });
        let response = update_site(
            ctx,
            state,
            authed_request(),
            web::Path::from("site-1".to_string()),
            body,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(ssl.lock().expect("capture lock poisoned").is_empty());
        assert!(theme.lock().expect("capture lock poisoned").is_empty());
    }

    #[tokio::test]
    async fn update_site_settings_merges_nested_and_flat_bodies() {
        let (ctx, state) = fixture();
        stored_site(&state, "site-1", "example.com");
        // Nested `{ "settings": {...} }` body: only the nested object
        // is merged into the stored settings.
        let response = update_site_settings(
            ctx.clone(),
            state.clone(),
            authed_request(),
            web::Path::from("site-1".to_string()),
            web::Json(json!({ "settings": { "theme": "dark", "locale": "id" } })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let settings = state
            .sites
            .read()
            .expect("saas lock poisoned")
            .get("site-1")
            .expect("site still stored")
            .settings
            .clone();
        assert_eq!(settings.get("theme"), Some(&json!("dark")));
        assert_eq!(settings.get("locale"), Some(&json!("id")));
        // Flat body: keys merge, and an explicit null deletes the key.
        let response = update_site_settings(
            ctx.clone(),
            state.clone(),
            authed_request(),
            web::Path::from("site-1".to_string()),
            web::Json(json!({ "theme": "light", "locale": null })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let settings = state
            .sites
            .read()
            .expect("saas lock poisoned")
            .get("site-1")
            .expect("site still stored")
            .settings
            .clone();
        assert_eq!(settings.get("theme"), Some(&json!("light")));
        assert!(!settings.contains_key("locale"));
        // A non-object `settings` value is still rejected.
        let response = update_site_settings(
            ctx,
            state,
            authed_request(),
            web::Path::from("site-1".to_string()),
            web::Json(json!({ "settings": "not-an-object" })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn write_handlers_reject_anonymous_callers() {
        for (label, status) in [
            ("suspend", {
                let (ctx, state) = fixture();
                stored_site(&state, "site-1", "example.com");
                suspend_site(
                    ctx,
                    state,
                    anonymous_request(),
                    web::Path::from("site-1".to_string()),
                )
                .await
                .status()
            }),
            ("activate", {
                let (ctx, state) = fixture();
                stored_site(&state, "site-1", "example.com");
                activate_site(
                    ctx,
                    state,
                    anonymous_request(),
                    web::Path::from("site-1".to_string()),
                )
                .await
                .status()
            }),
            ("delete", {
                let (ctx, state) = fixture();
                stored_site(&state, "site-1", "example.com");
                delete_site(
                    ctx,
                    state,
                    anonymous_request(),
                    web::Path::from("site-1".to_string()),
                )
                .await
                .status()
            }),
            ("update_settings", {
                let (ctx, state) = fixture();
                stored_site(&state, "site-1", "example.com");
                update_site_settings(
                    ctx,
                    state,
                    anonymous_request(),
                    web::Path::from("site-1".to_string()),
                    web::Json(json!({ "key": "value" })),
                )
                .await
                .status()
            }),
            ("get_settings", {
                let (ctx, state) = fixture();
                stored_site(&state, "site-1", "example.com");
                get_site_settings(
                    ctx,
                    state,
                    anonymous_request(),
                    web::Path::from("site-1".to_string()),
                )
                .await
                .status()
            }),
        ] {
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{label} must reject anonymous callers"
            );
        }
    }
}
