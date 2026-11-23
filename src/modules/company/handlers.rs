// Company module — company and site HTTP handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::{Company, CompanySite, NewCompany, NewCompanySite};
use crate::modules::network::DnsVerifier;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use uuid::Uuid;
use super::*;

// ---------------------------------------------------------------------------
// Company handlers.
// ---------------------------------------------------------------------------

pub(crate) async fn list_companies(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPANY_VIEW)
    })() {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<Company> = match db_pool(&ctx) {
            Some(pool) => {
                use unified_core::db::schema::companies::dsl as comp_dsl;
                let mut conn = pool.get().map_err(|error| {
                    UnifiedError::internal(format!("Cannot get database connection: {error}"))
                })?;
                comp_dsl::companies
                    .select(Company::as_select())
                    .load(&mut conn)
                    .map_err(|error| {
                        UnifiedError::internal(format!("Cannot load companies: {error}"))
                    })?
            }
            None => companies_store()
                .iter()
                .map(|entry| entry.value().clone())
                .collect(),
        };
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| {
                row.name.to_lowercase().contains(&needle)
                    || row.slug.to_lowercase().contains(&needle)
            });
        }
        if let Some(active) = params.filters.get("is_active") {
            let wanted = active == "true" || active == "1";
            items.retain(|row| row.is_active == wanted);
        }
        let sort = params.allowed_sort(&["name", "slug", "created_at"], "created_at");
        match sort.as_str() {
            "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
            "slug" => items.sort_by(|a, b| a.slug.cmp(&b.slug)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_company(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateCompanyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPANY_MANAGE)
    })() {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Company> = (|| {
        insert_company(&ctx, &body.name, body.slug.as_deref())
    })();
    match result {
        Ok(company) => json::success_response(company, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_company(ctx: web::Data<Arc<AppContext>>, req: HttpRequest, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPANY_VIEW)
    })() {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Company> = (|| {
        let id = id.into_inner();
        if let Some(pool) = db_pool(&ctx) {
            use unified_core::db::schema::companies::dsl as comp_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            if let Some(row) = comp_dsl::companies
                .filter(comp_dsl::id.eq(id))
                .select(Company::as_select())
                .first(&mut conn)
                .optional()
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot load company: {error}"))
                })?
            {
                return Ok(row);
            }
        }
        companies_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Company not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_company(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateCompanyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPANY_MANAGE)
    })() {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Company> = (|| {
        let id = id.into_inner();
        let mut company = load_company(&ctx, id)?;
        if let Some(name) = &body.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("name must not be empty"));
            }
            company.name = name.trim().to_string();
        }
        if let Some(slug) = &body.slug {
            let slug = slugify(slug);
            if slug.is_empty() {
                return Err(UnifiedError::bad_request("slug must not be empty"));
            }
            if company_slug_exists(&ctx, &slug, Some(id))? {
                return Err(UnifiedError::conflict(format!(
                    "A company with slug \"{slug}\" already exists"
                )));
            }
            company.slug = slug;
        }
        if let Some(active) = body.is_active {
            company.is_active = active;
        }
        company.updated_at = Utc::now();
        if let Some(pool) = db_pool(&ctx) {
            use unified_core::db::schema::companies::dsl as comp_dsl;
            #[derive(AsChangeset)]
            #[diesel(table_name = unified_core::db::schema::companies)]
            struct CompanyChanges {
                name: Option<String>,
                slug: Option<String>,
                is_active: Option<bool>,
            }
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            diesel::update(comp_dsl::companies.filter(comp_dsl::id.eq(id)))
                .set(CompanyChanges {
                    name: body.name.as_ref().map(|name| name.trim().to_string()),
                    slug: body.slug.clone().map(|slug| slugify(&slug)),
                    is_active: body.is_active,
                })
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot update company: {error}"))
                })?;
        }
        companies_store().insert(id, company.clone());
        Ok(company)
    })();
    json::respond(result, debug)
}

/// Load one company from the in-memory store, falling back to the
/// database (a DB-only row after a restart must not 404). A row
/// found in the database is cached in the store.
fn load_company(ctx: &AppContext, id: Uuid) -> UnifiedResult<Company> {
    if let Some(row) = companies_store()
        .get(&id)
        .map(|entry| entry.value().clone())
    {
        return Ok(row);
    }
    if let Some(pool) = db_pool(ctx) {
        use unified_core::db::schema::companies::dsl as comp_dsl;
        let mut conn = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot get database connection: {error}"))
        })?;
        if let Some(row) = comp_dsl::companies
            .filter(comp_dsl::id.eq(id))
            .select(Company::as_select())
            .first(&mut conn)
            .optional()
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load company: {error}"))
            })?
        {
            companies_store().insert(id, row.clone());
            return Ok(row);
        }
    }
    Err(UnifiedError::not_found("Company not found"))
}

/// Load one site from the in-memory store, falling back to the
/// database (a DB-only row after a restart must not 404). A row
/// found in the database is cached in the store.
fn load_site(ctx: &AppContext, id: Uuid) -> UnifiedResult<CompanySite> {
    if let Some(row) = sites_store().get(&id).map(|entry| entry.value().clone()) {
        return Ok(row);
    }
    if let Some(pool) = db_pool(ctx) {
        use unified_core::db::schema::company_sites::dsl as site_dsl;
        let mut conn = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot get database connection: {error}"))
        })?;
        if let Some(row) = site_dsl::company_sites
            .filter(site_dsl::id.eq(id))
            .select(CompanySite::as_select())
            .first(&mut conn)
            .optional()
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load company site: {error}"))
            })?
        {
            sites_store().insert(id, row.clone());
            return Ok(row);
        }
    }
    Err(UnifiedError::not_found("Company site not found"))
}

/// Normalise a domain for comparison: IDNA/punycode form, so a
/// unicode spelling and its `xn--` spelling of the same domain
/// compare equal.
fn normalize_domain_for_compare(domain: &str) -> String {
    DnsVerifier::check_domain_syntax(domain)
        .punycode
        .unwrap_or_else(|| domain.trim().to_lowercase())
}

pub(crate) async fn delete_company(ctx: web::Data<Arc<AppContext>>, req: HttpRequest, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPANY_MANAGE)
    })() {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        // Existence is decided BEFORE anything is deleted: a
        // DB-only row must delete cleanly, and a missing row must
        // 404 without touching the database.
        let _company = load_company(&ctx, id)?;
        if let Some(pool) = db_pool(&ctx) {
            use unified_core::db::schema::companies::dsl as comp_dsl;
            use unified_core::db::schema::company_sites::dsl as site_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            diesel::delete(site_dsl::company_sites.filter(site_dsl::company_id.eq(id)))
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot delete company sites: {error}"))
                })?;
            diesel::delete(comp_dsl::companies.filter(comp_dsl::id.eq(id)))
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot delete company: {error}"))
                })?;
        }
        sites_store().retain(|_, site| site.company_id != id);
        companies_store().remove(&id);
        Ok(serde_json::json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Site handlers.
// ---------------------------------------------------------------------------

pub(crate) async fn list_sites(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPANY_VIEW)
    })() {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<CompanySite> = match db_pool(&ctx) {
            Some(pool) => {
                use unified_core::db::schema::company_sites::dsl as site_dsl;
                let mut conn = pool.get().map_err(|error| {
                    UnifiedError::internal(format!("Cannot get database connection: {error}"))
                })?;
                site_dsl::company_sites
                    .select(CompanySite::as_select())
                    .load(&mut conn)
                    .map_err(|error| {
                        UnifiedError::internal(format!("Cannot load company sites: {error}"))
                    })?
            }
            None => sites_store()
                .iter()
                .map(|entry| entry.value().clone())
                .collect(),
        };
        if let Some(company_id) = params.filters.get("company_id") {
            if let Ok(wanted) = company_id.parse::<Uuid>() {
                items.retain(|row| row.company_id == wanted);
            }
        }
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| {
                row.name.to_lowercase().contains(&needle)
                    || row.domain.to_lowercase().contains(&needle)
            });
        }
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_site(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateSiteRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPANY_MANAGE)
    })() {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<CompanySite> = (|| {
        if !company_exists(&ctx, body.company_id) {
            return Err(UnifiedError::not_found("Company not found"));
        }
        if body.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("name is required"));
        }
        let syntax = DnsVerifier::check_domain_syntax(&body.domain);
        if !syntax.valid {
            return Err(UnifiedError::bad_request(format!(
                "Invalid site domain: {}",
                syntax.issues.join("; ")
            )));
        }
        let domain = syntax
            .punycode
            .clone()
            .unwrap_or_else(|| body.domain.trim().to_lowercase());
        let duplicate = site_domain_exists(&ctx, &domain)?;
        if duplicate {
            return Err(UnifiedError::conflict(format!(
                "A site with domain \"{domain}\" already exists"
            )));
        }
        let now = Utc::now();
        let site = CompanySite {
            id: Uuid::now_v7(),
            company_id: body.company_id,
            name: body.name.trim().to_string(),
            domain,
            is_primary: body.is_primary,
            is_active: true,
            created_at: now,
            updated_at: now,
        };
        if let Some(pool) = db_pool(&ctx) {
            use unified_core::db::schema::company_sites::dsl as site_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            diesel::insert_into(site_dsl::company_sites)
                .values(&NewCompanySite {
                    id: site.id,
                    company_id: site.company_id,
                    name: site.name.clone(),
                    domain: site.domain.clone(),
                    is_primary: site.is_primary,
                })
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot create company site: {error}"))
                })?;
        }
        sites_store().insert(site.id, site.clone());
        if site.is_primary {
            demote_other_primaries(&ctx, site.company_id, site.id)?;
        }
        Ok(site)
    })();
    match result {
        Ok(site) => json::success_response(site, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_site(ctx: web::Data<Arc<AppContext>>, req: HttpRequest, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPANY_VIEW)
    })() {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<CompanySite> = (|| {
        let id = id.into_inner();
        if let Some(pool) = db_pool(&ctx) {
            use unified_core::db::schema::company_sites::dsl as site_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            if let Some(row) = site_dsl::company_sites
                .filter(site_dsl::id.eq(id))
                .select(CompanySite::as_select())
                .first(&mut conn)
                .optional()
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot load company site: {error}"))
                })?
            {
                return Ok(row);
            }
        }
        sites_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Company site not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_site(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateSiteRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPANY_MANAGE)
    })() {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<CompanySite> = (|| {
        let id = id.into_inner();
        let mut site = load_site(&ctx, id)?;
        // The site domain is immutable after creation. Both sides
        // are normalised to punycode first, so a unicode spelling
        // of the same domain is not rejected as a change.
        if let Some(domain) = &body.domain {
            if normalize_domain_for_compare(domain) != normalize_domain_for_compare(&site.domain)
            {
                return Err(UnifiedError::conflict(
                    "The site domain is immutable and cannot be changed after creation",
                ));
            }
        }
        if let Some(name) = &body.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("name must not be empty"));
            }
            site.name = name.trim().to_string();
        }
        if let Some(primary) = body.is_primary {
            site.is_primary = primary;
        }
        if let Some(active) = body.is_active {
            site.is_active = active;
        }
        site.updated_at = Utc::now();
        if let Some(pool) = db_pool(&ctx) {
            use unified_core::db::schema::company_sites::dsl as site_dsl;
            #[derive(AsChangeset)]
            #[diesel(table_name = unified_core::db::schema::company_sites)]
            struct SiteChanges {
                name: Option<String>,
                is_primary: Option<bool>,
                is_active: Option<bool>,
            }
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            diesel::update(site_dsl::company_sites.filter(site_dsl::id.eq(id)))
                .set(SiteChanges {
                    name: body.name.as_ref().map(|name| name.trim().to_string()),
                    is_primary: body.is_primary,
                    is_active: body.is_active,
                })
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot update company site: {error}"))
                })?;
        }
        sites_store().insert(id, site.clone());
        if site.is_primary {
            demote_other_primaries(&ctx, site.company_id, site.id)?;
        }
        Ok(site)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_site(ctx: web::Data<Arc<AppContext>>, req: HttpRequest, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPANY_MANAGE)
    })() {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        // Existence is decided BEFORE anything is deleted: a
        // DB-only row must delete cleanly, and a missing row must
        // 404 without touching the database.
        let _site = load_site(&ctx, id)?;
        if let Some(pool) = db_pool(&ctx) {
            use unified_core::db::schema::company_sites::dsl as site_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            diesel::delete(site_dsl::company_sites.filter(site_dsl::id.eq(id)))
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot delete company site: {error}"))
                })?;
        }
        sites_store().remove(&id);
        Ok(serde_json::json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_company_sites(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPANY_VIEW)
    })() {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        if !company_exists(&ctx, id) {
            return Err(UnifiedError::not_found("Company not found"));
        }
        let mut items: Vec<CompanySite> = match db_pool(&ctx) {
            Some(pool) => {
                use unified_core::db::schema::company_sites::dsl as site_dsl;
                let mut conn = pool.get().map_err(|error| {
                    UnifiedError::internal(format!("Cannot get database connection: {error}"))
                })?;
                site_dsl::company_sites
                    .filter(site_dsl::company_id.eq(id))
                    .select(CompanySite::as_select())
                    .load(&mut conn)
                    .map_err(|error| {
                        UnifiedError::internal(format!("Cannot load company sites: {error}"))
                    })?
            }
            None => sites_store()
                .iter()
                .filter(|entry| entry.value().company_id == id)
                .map(|entry| entry.value().clone())
                .collect(),
        };
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| {
                row.name.to_lowercase().contains(&needle)
                    || row.domain.to_lowercase().contains(&needle)
            });
        }
        items.sort_by(|a, b| (b.is_primary, a.created_at).cmp(&(a.is_primary, b.created_at)));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> AppContext {
        use unified_core::config::AppConfig;
        use unified_core::modules::ModuleRegistry;
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    #[test]
    fn load_company_finds_a_store_row_and_404s_when_missing() {
        let ctx = test_ctx();
        let id = Uuid::new_v4();
        assert_eq!(load_company(&ctx, id).unwrap_err().code, 404);
        let now = Utc::now();
        let slug = format!("acme-{}", Uuid::new_v4().simple());
        companies_store().insert(
            id,
            Company {
                id,
                name: "Acme".to_string(),
                slug: slug.clone(),
                is_active: true,
                created_at: now,
                updated_at: now,
            },
        );
        let found = load_company(&ctx, id).unwrap();
        assert_eq!(found.slug, slug);
        companies_store().remove(&id);
    }

    #[test]
    fn load_site_finds_a_store_row_and_404s_when_missing() {
        let ctx = test_ctx();
        let id = Uuid::new_v4();
        assert_eq!(load_site(&ctx, id).unwrap_err().code, 404);
        let now = Utc::now();
        sites_store().insert(
            id,
            CompanySite {
                id,
                company_id: Uuid::new_v4(),
                name: "Main".to_string(),
                domain: format!("{}.example.invalid", Uuid::new_v4().simple()),
                is_primary: true,
                is_active: true,
                created_at: now,
                updated_at: now,
            },
        );
        assert_eq!(load_site(&ctx, id).unwrap().id, id);
        sites_store().remove(&id);
    }

    #[test]
    fn domain_comparison_treats_unicode_and_punycode_as_the_same_domain() {
        assert_eq!(
            normalize_domain_for_compare("münchen.de"),
            normalize_domain_for_compare("xn--mnchen-3ya.de")
        );
        assert_eq!(normalize_domain_for_compare("EXAMPLE.COM"), "example.com");
        assert_ne!(
            normalize_domain_for_compare("example.com"),
            normalize_domain_for_compare("other.com")
        );
    }
}
