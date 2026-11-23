// Domains module - HTTP handlers.
//
// Split from `src/modules/domains.rs`; the module root re-exports the public API of this module.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::events;
use unified_core::json;
use unified_core::pagination::PaginationParams;
use unified_core::db::models::{Domain, NewDomain};
use crate::modules::company::{company_exists, site_exists};
use crate::modules::network::DnsVerifier;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use diesel::prelude::*;
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

use super::service::{domain_exists_by_name, is_unique_violation, load_domain, persist_domain, valid_ssl_status, verify_domain_record};
use super::state::{PERM_DOMAINS_MANAGE, PERM_DOMAINS_VIEW, caller, caller_company, require_company_for_write, db_pool, domains_store, in_company_scope, paginate, require_company_match_uuid, require_permission, require_record_in_company_scope, require_tenant};
use super::types::{CreateDomainRequest, CreateSubdomainRequest, UpdateDomainRequest, VerifyRequest};

// ---------------------------------------------------------------------------
// Handlers.
//
// Every handler enforces a permission through the `role.check` hook
// first (`domains.view` for reads, `domains.manage` for writes),
// failing closed when the caller has no role or the check is
// unanswered.
// ---------------------------------------------------------------------------

pub(crate) async fn list_domains(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_DOMAINS_VIEW)?;
        let scope = caller_company(&_tenant, &req)?;
        let mut items: Vec<Domain> = match db_pool(&ctx) {
            Some(pool) => {
                use unified_core::db::schema::domains::dsl as dom_dsl;
                let mut conn = pool.get().map_err(|error| {
                    UnifiedError::internal(format!("Cannot get database connection: {error}"))
                })?;
                dom_dsl::domains
                    .select(Domain::as_select())
                    .load(&mut conn)
                    .map_err(|error| {
                        UnifiedError::internal(format!("Cannot load domains: {error}"))
                    })?
            }
            None => domains_store()
                .iter()
                .map(|entry| entry.value().clone())
                .collect(),
        };
        // Company scope first: a scoped caller sees only their own
        // company's domains plus global (unscoped) ones; an
        // unresolved caller sees global domains only.
        items.retain(|row| in_company_scope(&row.company_id, &scope));
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| row.name.to_lowercase().contains(&needle));
        }
        if let Some(status) = params.filters.get("ssl_status") {
            items.retain(|row| row.ssl_status == *status);
        }
        if let Some(company_id) = params.filters.get("company_id") {
            if let Ok(wanted) = company_id.parse::<Uuid>() {
                items.retain(|row| row.company_id == Some(wanted));
            }
        }
        let sort = params.allowed_sort(&["name", "ssl_status", "created_at"], "created_at");
        match sort.as_str() {
            "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
            "ssl_status" => items.sort_by(|a, b| a.ssl_status.cmp(&b.ssl_status)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_domain(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateDomainRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Domain> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_DOMAINS_MANAGE)?;
        let company_id = require_company_match_uuid(&_tenant, &req, &body.company_id)?;
        let syntax = DnsVerifier::check_domain_syntax(&body.name);
        if !syntax.valid {
            return Err(UnifiedError::bad_request(format!(
                "Invalid domain: {}",
                syntax.issues.join("; ")
            )));
        }
        let name = syntax
            .punycode
            .clone()
            .unwrap_or_else(|| body.name.trim().to_lowercase());
        if let Some(company_id) = company_id {
            if !company_exists(&ctx, company_id) {
                return Err(UnifiedError::not_found("Company not found"));
            }
        }
        if let Some(site_id) = body.site_id {
            if !site_exists(&ctx, site_id) {
                return Err(UnifiedError::not_found("Company site not found"));
            }
        }
        // Duplicate check against the store *and* the database, so a
        // duplicate still answers 409 after a restart, when the store
        // starts empty.
        if domain_exists_by_name(&ctx, &name, None)? {
            return Err(UnifiedError::conflict(format!(
                "Domain \"{name}\" already exists"
            )));
        }
        let now = Utc::now();
        let domain = Domain {
            id: Uuid::now_v7(),
            name,
            company_id,
            site_id: body.site_id,
            is_active: true,
            ssl_status: "none".to_string(),
            ssl_expires_at: None,
            created_at: now,
            updated_at: now,
        };
        if let Some(pool) = db_pool(&ctx) {
            use unified_core::db::schema::domains::dsl as dom_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            diesel::insert_into(dom_dsl::domains)
                .values(&NewDomain {
                    id: domain.id,
                    name: domain.name.clone(),
                    company_id: domain.company_id,
                    site_id: domain.site_id,
                })
                .execute(&mut conn)
                .map_err(|error| {
                    // The pre-check can race with a concurrent insert;
                    // the database unique constraint is the authority.
                    if is_unique_violation(&error) {
                        UnifiedError::conflict(format!(
                            "Domain \"{}\" already exists",
                            domain.name
                        ))
                    } else {
                        UnifiedError::internal(format!("Cannot create domain: {error}"))
                    }
                })?;
        }
        domains_store().insert(domain.id, domain.clone());
        Ok(domain)
    })();
    match result {
        Ok(domain) => json::success_response(domain, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_domain(req: HttpRequest, ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Domain> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_DOMAINS_VIEW)?;
        let scope = caller_company(&_tenant, &req)?;
        let id = id.into_inner();
        if let Some(pool) = db_pool(&ctx) {
            use unified_core::db::schema::domains::dsl as dom_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            if let Some(row) = dom_dsl::domains
                .filter(dom_dsl::id.eq(id))
                .select(Domain::as_select())
                .first(&mut conn)
                .optional()
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot load domain: {error}"))
                })?
            {
                require_record_in_company_scope(&row.company_id, &scope)?;
                return Ok(row);
            }
        }
        let domain = domains_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Domain not found"))?;
        require_record_in_company_scope(&domain.company_id, &scope)?;
        Ok(domain)
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_domain(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    body: web::Json<UpdateDomainRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Domain> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_DOMAINS_MANAGE)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let id = id.into_inner();
        // Database first, store fallback: a domain present in the
        // database but not in the store (for example after a restart)
        // must not answer 404.
        let mut domain = load_domain(&ctx, id)?;
        require_record_writable_in_company_scope(&domain.company_id, &scope)?;
        if let Some(name) = &body.name {
            let syntax = DnsVerifier::check_domain_syntax(name);
            if !syntax.valid {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid domain: {}",
                    syntax.issues.join("; ")
                )));
            }
            let name = syntax
                .punycode
                .clone()
                .unwrap_or_else(|| name.trim().to_lowercase());
            if domain_exists_by_name(&ctx, &name, Some(id))? {
                return Err(UnifiedError::conflict(format!(
                    "Domain \"{name}\" already exists"
                )));
            }
            domain.name = name;
        }
        if let Some(active) = body.is_active {
            domain.is_active = active;
        }
        if let Some(status) = &body.ssl_status {
            // Client-supplied statuses are allowlisted: an arbitrary
            // string must never land in the database.
            if !valid_ssl_status(status) {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid ssl_status \"{status}\": expected one of none, active, expiring, expired, renewing, unreachable"
                )));
            }
            domain.ssl_status = status.clone();
        }
        domain.updated_at = Utc::now();
        // A persistence failure is an error, not a silent divergence
        // between the store and the database.
        persist_domain(&ctx, &domain)?;
        Ok(domain)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_domain(req: HttpRequest, ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_DOMAINS_MANAGE)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let id = id.into_inner();
        // Existence is confirmed *before* any delete runs: deleting
        // the database row first and answering 404 afterwards would
        // destroy a row the caller was told does not exist.
        let domain = load_domain(&ctx, id)?;
        require_record_writable_in_company_scope(&domain.company_id, &scope)?;
        if let Some(pool) = db_pool(&ctx) {
            use unified_core::db::schema::domains::dsl as dom_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            diesel::delete(dom_dsl::domains.filter(dom_dsl::id.eq(id)))
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot delete domain: {error}"))
                })?;
        }
        domains_store().remove(&id);
        Ok(serde_json::json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn verify_domain(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    body: Option<web::Json<VerifyRequest>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_DOMAINS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let scope = match require_company_for_write(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let id = id.into_inner();
    let selector = body.as_ref().and_then(|body| body.selector.clone());
    // DNS checks via the Network module's verifier (also published
    // as a `dns.check` event by the verifier endpoint; the full
    // report is published here as `domain.verify`).
    let result: UnifiedResult<Value> = (|| async {
        let domain = load_domain(&ctx, id)?;
        require_record_writable_in_company_scope(&domain.company_id, &scope)?;
        verify_domain_record(&domain, selector.as_deref(), db_pool(&ctx), true).await
    })()
    .await;
    match result {
        Ok(outcome) => {
            ctx.hooks.emit("domain.verify", outcome.clone());
            json::success_response(outcome, 200)
        }
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn ssl_renew_handler(req: HttpRequest, ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Domain> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_DOMAINS_MANAGE)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let id = id.into_inner();
        let mut domain = load_domain(&ctx, id)?;
        require_record_writable_in_company_scope(&domain.company_id, &scope)?;
        domain.ssl_status = "renewing".to_string();
        domain.updated_at = Utc::now();
        persist_domain(&ctx, &domain)?;
        // Hand the actual renewal to the SSL manager (ACME); it reports
        // back through the `ssl.renew` event with `ssl_expires_at`.
        ctx.hooks.emit(
            events::SSL_RENEW,
            serde_json::json!({
                "domain_id": domain.id,
                "domain": domain.name,
                "source": "domains",
            }),
        );
        Ok(domain)
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_subdomain(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    body: web::Json<CreateSubdomainRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Domain> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_DOMAINS_MANAGE)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let id = id.into_inner();
        let parent = load_domain(&ctx, id)?;
        require_record_in_company_scope(&parent.company_id, &scope)?;
        let label = body.name.trim().to_lowercase();
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(UnifiedError::bad_request(
                "Subdomain name must be a valid DNS label (1-63 characters, letters, digits, hyphens)",
            ));
        }
        let name = format!("{label}.{}", parent.name);
        if domain_exists_by_name(&ctx, &name, None)? {
            return Err(UnifiedError::conflict(format!(
                "Domain \"{name}\" already exists"
            )));
        }
        let now = Utc::now();
        let subdomain = Domain {
            id: Uuid::now_v7(),
            name,
            company_id: parent.company_id,
            site_id: parent.site_id,
            is_active: true,
            ssl_status: "none".to_string(),
            ssl_expires_at: None,
            created_at: now,
            updated_at: now,
        };
        if let Some(pool) = db_pool(&ctx) {
            use unified_core::db::schema::domains::dsl as dom_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            diesel::insert_into(dom_dsl::domains)
                .values(&NewDomain {
                    id: subdomain.id,
                    name: subdomain.name.clone(),
                    company_id: subdomain.company_id,
                    site_id: subdomain.site_id,
                })
                .execute(&mut conn)
                .map_err(|error| {
                    if is_unique_violation(&error) {
                        UnifiedError::conflict(format!(
                            "Domain \"{}\" already exists",
                            subdomain.name
                        ))
                    } else {
                        UnifiedError::internal(format!("Cannot create subdomain: {error}"))
                    }
                })?;
        }
        domains_store().insert(subdomain.id, subdomain.clone());
        Ok(subdomain)
    })();
    match result {
        Ok(domain) => json::success_response(domain, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

/// Resolve the caller's company for a WRITE (C3 task 315, strict by
/// default): a write requires a resolved company scope, so an
/// unresolved caller is refused with 403 instead of silently
/// writing a global (unscoped) record. Mutation handlers call
/// this instead of `caller_company`; read handlers keep
/// `caller_company`, and the named carve-outs (self-service
/// writes, settings with no company field, scan/test triggers,
/// platform surfaces) keep it too.
pub(crate) fn require_company_for_write(
    tenant: &unified_core::tenancy::TenantContext,
    req: &actix_web::HttpRequest,
) -> UnifiedResult<Option<String>> {
    match super::state::caller_company(tenant, req)? {
        Some(company) => Ok(Some(company)),
        None => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}
