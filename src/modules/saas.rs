// SaaS multi-site module.
//
// Each site belongs to a company, has its own domain, theme, SSL status,
// and per-site settings namespace. Creating a site runs the provisioning
// flow: company check, DNS verification, site creation, then wiring of
// theme and SSL — all through the hook bus, never through direct calls
// to other modules.
//
// Routes (all under `/api/v1/saas`):
//   GET    /sites                          - paginated sites (filter by company_id, status, domain_status; search)
//   POST   /sites                          - provision a site {company_id, name, domain, theme_id}
//   GET    /sites/{id}                     - one site (domain status, theme, SSL status)
//   PUT    /sites/{id}                     - update a site
//   DELETE /sites/{id}                     - delete a site
//   POST   /sites/{id}/suspend            - suspend a site
//   POST   /sites/{id}/activate           - activate a suspended site
//   GET    /sites/{id}/settings            - per-site settings namespace
//   PUT    /sites/{id}/settings            - merge per-site settings
//
// Hook contracts:
//   Provisioning emits `company.exists` (Company module) and `dns.verify`
//   (DNS verifier). When no handler answers, the site is still created
//   and the response carries a `warnings` array saying so, honestly:
//   a missing company handler means the company was accepted without
//   verification; a missing DNS handler means the domain was accepted
//   unchecked (`domain_status` becomes `unchecked`).
//   After creation the module emits `site.provisioned`, `ssl.renew`
//   (core event) for the domain, and `theme.activate_requested` for the
//   theme. Suspend / activate emit `site.suspended` / `site.activated`.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus};
use crate::core::json as api_json;
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::sync::OnceLock;
use uuid::Uuid;

pub const EVENT_COMPANY_EXISTS: &str = "company.exists";
pub const EVENT_DNS_VERIFY: &str = "dns.verify";
pub const EVENT_SITE_PROVISIONED: &str = "site.provisioned";
pub const EVENT_THEME_ACTIVATE_REQUESTED: &str = "theme.activate_requested";
pub const EVENT_SITE_SUSPENDED: &str = "site.suspended";
pub const EVENT_SITE_ACTIVATED: &str = "site.activated";

fn domain_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"^(?=.{1,253}$)[a-zA-Z0-9]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(\.[a-zA-Z0-9]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)+$")
            .expect("domain regex must compile")
    })
}

/// Validate a hostname: valid labels, at least one dot, alphabetic TLD.
fn validate_domain(domain: &str) -> UnifiedResult<()> {
    if !domain_regex().is_match(domain) {
        return Err(UnifiedError::bad_request(format!(
            "Domain '{domain}' is not a valid hostname"
        )));
    }
    let tld = domain.rsplit('.').next().unwrap_or("");
    if tld.len() < 2 || !tld.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(format!(
            "Domain '{domain}' must end with an alphabetic top-level domain"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct Site {
    pub id: String,
    pub company_id: String,
    pub name: String,
    pub domain: String,
    pub theme_id: String,
    /// `active`, `suspended`, or `provisioning`.
    pub status: String,
    /// `verified`, `unverified`, or `unchecked`.
    pub domain_status: String,
    /// `pending`, `active`, `failed`, or `none`.
    pub ssl_status: String,
    /// Per-site settings namespace.
    pub settings: HashMap<String, Value>,
    pub created_at: String,
    pub updated_at: String,
}

pub struct SaasState {
    sites: RwLock<HashMap<String, Site>>,
    hooks: RwLock<Option<Arc<HookBus>>>,
    debug: AtomicBool,
}

impl SaasState {
    fn new() -> Self {
        Self {
            sites: RwLock::new(HashMap::new()),
            hooks: RwLock::new(None),
            debug: AtomicBool::new(false),
        }
    }

    fn set_hooks(&self, hooks: Arc<HookBus>) {
        *self.hooks.write().expect("saas hooks lock poisoned") = Some(hooks);
    }

    fn hooks(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("saas hooks lock poisoned").clone()
    }

    fn is_debug(&self) -> bool {
        self.debug.load(Ordering::SeqCst)
    }

    fn get_site(&self, id: &str) -> Option<Site> {
        self.sites
            .read()
            .expect("saas lock poisoned")
            .get(id)
            .cloned()
    }

    fn domain_taken(&self, domain: &str, except_id: Option<&str>) -> bool {
        self.sites
            .read()
            .expect("saas lock poisoned")
            .values()
            .any(|site| site.domain == domain && Some(site.id.as_str()) != except_id)
    }

    /// Ask the Company module whether a company exists. Returns
    /// `Ok(true)` / `Ok(false)` when it answers, `Ok(None)` when no
    /// handler is registered for `company.exists`.
    fn company_exists(&self, company_id: &str) -> Option<bool> {
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
    fn dns_verify(&self, domain: &str) -> (Option<bool>, Option<String>) {
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

    fn emit(&self, event: &str, data: Value) {
        if let Some(hooks) = self.hooks() {
            hooks.emit(event, data);
        }
    }
}

#[derive(Debug, Deserialize)]
struct SiteInput {
    company_id: String,
    name: String,
    domain: String,
    theme_id: String,
}

#[derive(Debug, Deserialize)]
struct SiteUpdateInput {
    company_id: Option<String>,
    name: Option<String>,
    domain: Option<String>,
    theme_id: Option<String>,
}

fn not_found_site(debug: bool) -> HttpResponse {
    api_json::error_response(&UnifiedError::not_found("Site not found"), debug)
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

async fn list_sites(
    state: web::Data<Arc<SaasState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
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

async fn create_site(
    state: web::Data<Arc<SaasState>>,
    body: web::Json<SiteInput>,
) -> HttpResponse {
    let state: Arc<SaasState> = state.into_inner();
    let debug = state.is_debug();
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

    // 3. Create the site.
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
    {
        let mut sites = state.sites.write().expect("saas lock poisoned");
        sites.insert(site.id.clone(), site.clone());
    }

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
    state.emit(
        EVENT_THEME_ACTIVATE_REQUESTED,
        json!({ "theme_id": site.theme_id, "site_id": site.id }),
    );

    let mut data = serde_json::to_value(&site).expect("site serializes");
    if !warnings.is_empty() {
        data.as_object_mut()
            .expect("site value is an object")
            .insert("warnings".to_string(), json!(warnings));
    }
    api_json::success_response(data, 201)
}

async fn get_site(
    state: web::Data<Arc<SaasState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    match state.get_site(&path.into_inner()) {
        Some(site) => api_json::success_response(site, 200),
        None => not_found_site(debug),
    }
}

async fn update_site(
    state: web::Data<Arc<SaasState>>,
    path: web::Path<String>,
    body: web::Json<SiteUpdateInput>,
) -> HttpResponse {
    let state: Arc<SaasState> = state.into_inner();
    let debug = state.is_debug();
    let id = path.into_inner();
    let input = body.into_inner();
    if let Some(name) = &input.name {
        if name.trim().is_empty() {
            return api_json::error_response(
                &UnifiedError::bad_request("Site name must not be empty"),
                debug,
            );
        }
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
    let site = match sites.get_mut(&id) {
        Some(site) => site,
        None => return not_found_site(debug),
    };
    if let Some(company_id) = input.company_id {
        site.company_id = company_id.trim().to_string();
    }
    if let Some(name) = input.name {
        site.name = name.trim().to_string();
    }
    if let Some(domain) = input.domain {
        site.domain = domain.trim().to_lowercase();
        // A changed domain must be re-verified on the next provisioning
        // step; mark it unchecked until the DNS verifier confirms it.
        site.domain_status = "unchecked".to_string();
        site.ssl_status = "pending".to_string();
    }
    if let Some(theme_id) = input.theme_id {
        site.theme_id = theme_id.trim().to_string();
    }
    site.updated_at = Utc::now().to_rfc3339();
    let site = site.clone();
    api_json::success_response(site, 200)
}

async fn delete_site(
    state: web::Data<Arc<SaasState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    let removed = state
        .sites
        .write()
        .expect("saas lock poisoned")
        .remove(&path.into_inner());
    match removed {
        Some(site) => api_json::success_response(json!({ "deleted": true, "id": site.id }), 200),
        None => not_found_site(debug),
    }
}

async fn suspend_site(
    state: web::Data<Arc<SaasState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let state: Arc<SaasState> = state.into_inner();
    let debug = state.is_debug();
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

async fn activate_site(
    state: web::Data<Arc<SaasState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let state: Arc<SaasState> = state.into_inner();
    let debug = state.is_debug();
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

async fn get_site_settings(
    state: web::Data<Arc<SaasState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    match state.get_site(&path.into_inner()) {
        Some(site) => api_json::success_response(
            json!({ "site_id": site.id, "settings": site.settings }),
            200,
        ),
        None => not_found_site(debug),
    }
}

async fn update_site_settings(
    state: web::Data<Arc<SaasState>>,
    path: web::Path<String>,
    body: web::Json<Value>,
) -> HttpResponse {
    let debug = state.is_debug();
    let id = path.into_inner();
    let body = body.into_inner();
    // Accept either `{ "settings": { ... } }` or a flat settings object.
    let incoming: HashMap<String, Value> = match &body {
        Value::Object(map) if map.contains_key("settings") => {
            match map.get("settings") {
                Some(Value::Object(settings)) => settings
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
                _ => {
                    return api_json::error_response(
                        &UnifiedError::bad_request("'settings' must be a JSON object"),
                        debug,
                    )
                }
            }
        }
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
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

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct SaasModule {
    state: Arc<SaasState>,
}

impl SaasModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(SaasState::new()),
        }
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SaasModule::new()));
}

impl Module for SaasModule {
    fn id(&self) -> &'static str {
        "saas"
    }

    fn name(&self) -> &'static str {
        "SaaS Multi-Site"
    }

    fn description(&self) -> &'static str {
        "SaaS multi-site provisioning: per-company sites with their own domain, theme, SSL status, and settings namespace."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["company", "domains", "theme"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        self.state.debug.store(ctx.is_debug(), Ordering::SeqCst);
        self.state.set_hooks(ctx.hooks.clone());
        Ok(())
    }

    fn register_hooks(&self, _bus: &HookBus) {}

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        let state = self.state.clone();
        cfg.service(
            web::scope("/api/v1/saas")
                .app_data(web::Data::new(state))
                .route("/sites", web::get().to(list_sites))
                .route("/sites", web::post().to(create_site))
                .route("/sites/{id}", web::get().to(get_site))
                .route("/sites/{id}", web::put().to(update_site))
                .route("/sites/{id}", web::delete().to(delete_site))
                .route("/sites/{id}/suspend", web::post().to(suspend_site))
                .route("/sites/{id}/activate", web::post().to(activate_site))
                .route("/sites/{id}/settings", web::get().to(get_site_settings))
                .route("/sites/{id}/settings", web::put().to(update_site_settings)),
        );
    }
}
