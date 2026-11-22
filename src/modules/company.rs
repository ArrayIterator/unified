// Company module — multi-site, multi-company management.
//
// A company owns sites. A site's domain is IMMUTABLE after creation:
// any update that tries to change it is rejected with HTTP 409.
// Exactly one site per company is primary: marking a site primary
// demotes every other site of the same company.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::db::models::{Company, CompanySite, NewCompany, NewCompanySite};
use crate::modules::network::DnsVerifier;
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// In-memory stores (used when no database pool is attached).
// ---------------------------------------------------------------------------

static COMPANIES: OnceLock<DashMap<Uuid, Company>> = OnceLock::new();
static SITES: OnceLock<DashMap<Uuid, CompanySite>> = OnceLock::new();

pub(crate) fn companies_store() -> &'static DashMap<Uuid, Company> {
    COMPANIES.get_or_init(DashMap::new)
}

pub(crate) fn sites_store() -> &'static DashMap<Uuid, CompanySite> {
    SITES.get_or_init(DashMap::new)
}

/// True when a company with this id exists (in-memory or database).
pub fn company_exists(ctx: &AppContext, id: Uuid) -> bool {
    if companies_store().contains_key(&id) {
        return true;
    }
    if let Some(pool) = db_pool(ctx) {
        use crate::db::schema::companies::dsl as comp_dsl;
        if let Ok(mut conn) = pool.get() {
            if let Ok(Some(_)) = comp_dsl::companies
                .filter(comp_dsl::id.eq(id))
                .select(Company::as_select())
                .first(&mut conn)
                .optional()
            {
                return true;
            }
        }
    }
    false
}

/// True when a company site with this id exists.
pub fn site_exists(ctx: &AppContext, id: Uuid) -> bool {
    if sites_store().contains_key(&id) {
        return true;
    }
    if let Some(pool) = db_pool(ctx) {
        use crate::db::schema::company_sites::dsl as site_dsl;
        if let Ok(mut conn) = pool.get() {
            if let Ok(Some(_)) = site_dsl::company_sites
                .filter(site_dsl::id.eq(id))
                .select(CompanySite::as_select())
                .first(&mut conn)
                .optional()
            {
                return true;
            }
        }
    }
    false
}

fn db_pool(ctx: &AppContext) -> Option<crate::db::DbPool> {
    ctx.db.read().expect("db lock poisoned").clone()
}

// ---------------------------------------------------------------------------
// Hook context: `init` stashes the shared application context so the
// synchronous hook handlers below can use the database. Hook handlers
// read it lazily, so registration order (hooks before `init`) does not
// matter. The stashed context shares the live pool/config locks; only
// the uptime and pause flags are snapshots.
// ---------------------------------------------------------------------------

static COMPANY_CTX: OnceLock<Arc<AppContext>> = OnceLock::new();

fn stash_ctx(ctx: &AppContext) -> Arc<AppContext> {
    Arc::new(AppContext {
        config: ctx.config.clone(),
        hooks: ctx.hooks.clone(),
        modules: ctx.modules.clone(),
        cache: ctx.cache.clone(),
        geo: ctx.geo.clone(),
        db: ctx.db.clone(),
        start_time: std::time::Instant::now(),
        paused: std::sync::atomic::AtomicBool::new(ctx.is_paused()),
        maintenance: std::sync::atomic::AtomicBool::new(ctx.is_maintenance()),
    })
}

fn hook_ctx() -> Option<Arc<AppContext>> {
    COMPANY_CTX.get().cloned()
}

/// Create and store one company. Shared by the HTTP create handler and
/// the `company.create_requested` hook handler (CRM lead conversion).
/// The slug is derived from the name when none is supplied; a duplicate
/// slug is a conflict.
fn insert_company(
    ctx: &AppContext,
    name: &str,
    slug: Option<&str>,
) -> UnifiedResult<Company> {
    if name.trim().is_empty() {
        return Err(UnifiedError::bad_request("name is required"));
    }
    let slug = match slug {
        Some(slug) if !slug.trim().is_empty() => slugify(slug),
        _ => slugify(name),
    };
    if slug.is_empty() {
        return Err(UnifiedError::bad_request(
            "Could not derive a slug from the company name",
        ));
    }
    let duplicate = companies_store()
        .iter()
        .any(|entry| entry.value().slug == slug);
    if duplicate {
        return Err(UnifiedError::conflict(format!(
            "A company with slug \"{slug}\" already exists"
        )));
    }
    let now = Utc::now();
    let company = Company {
        id: Uuid::now_v7(),
        name: name.trim().to_string(),
        slug: slug.clone(),
        is_active: true,
        created_at: now,
        updated_at: now,
    };
    if let Some(pool) = db_pool(ctx) {
        use crate::db::schema::companies::dsl as comp_dsl;
        let mut conn = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot get database connection: {error}"))
        })?;
        diesel::insert_into(comp_dsl::companies)
            .values(&NewCompany {
                id: company.id,
                name: company.name.clone(),
                slug: company.slug.clone(),
            })
            .execute(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot create company: {error}"))
            })?;
    }
    companies_store().insert(company.id, company.clone());
    Ok(company)
}

/// Answer `company.exists` (from the SaaS module) with `{"exists": bool}`.
fn on_company_exists(ctx: &mut HookContext) {
    let id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<Uuid>().ok());
    let exists = match (id, hook_ctx()) {
        (Some(id), Some(app_ctx)) => company_exists(&app_ctx, id),
        (Some(id), None) => companies_store().contains_key(&id),
        (None, _) => false,
    };
    ctx.add_result(json!({ "exists": exists }));
}

/// Create the company requested by a CRM lead conversion
/// (`company.create_requested`, payload `{ "name": ..., "slug"? }`).
fn on_company_create_requested(ctx: &mut HookContext) {
    let Some(app_ctx) = hook_ctx() else {
        ctx.add_result(json!({
            "error": "company module is not initialised",
        }));
        return;
    };
    let name = ctx
        .get("name")
        .and_then(|value| value.as_str())
        .or_else(|| ctx.get("company").and_then(|value| value.as_str()))
        .unwrap_or("")
        .to_string();
    let slug = ctx
        .get("slug")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    match insert_company(&app_ctx, &name, slug.as_deref()) {
        Ok(company) => {
            ctx.add_result(json!({
                "company_id": company.id,
                "slug": company.slug,
            }));
        }
        Err(error) => {
            ctx.add_result(json!({ "error": error.message }));
        }
    }
}

fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in name.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

/// Demote every other primary site of a company (in-memory and database).
fn demote_other_primaries(ctx: &AppContext, company_id: Uuid, keep_id: Uuid) {
    for entry in sites_store().iter_mut() {
        if entry.value().company_id == company_id && entry.key() != &keep_id && entry.is_primary {
            entry.is_primary = false;
            entry.updated_at = Utc::now();
        }
    }
    if let Some(pool) = db_pool(ctx) {
        use crate::db::schema::company_sites::dsl as site_dsl;
        #[derive(AsChangeset)]
        #[diesel(table_name = crate::db::schema::company_sites)]
        struct PrimaryChange {
            is_primary: bool,
        }
        if let Ok(mut conn) = pool.get() {
            let _ = diesel::update(
                site_dsl::company_sites
                    .filter(site_dsl::company_id.eq(company_id))
                    .filter(site_dsl::id.ne(keep_id)),
            )
            .set(PrimaryChange { is_primary: false })
            .execute(&mut conn);
        }
    }
}

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateCompanyRequest {
    name: String,
    #[serde(default)]
    slug: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateCompanyRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CreateSiteRequest {
    company_id: Uuid,
    name: String,
    domain: String,
    #[serde(default)]
    is_primary: bool,
}

#[derive(Debug, Deserialize)]
struct UpdateSiteRequest {
    #[serde(default)]
    name: Option<String>,
    /// The site domain is immutable: sending a different value is a 409.
    #[serde(default)]
    domain: Option<String>,
    #[serde(default)]
    is_primary: Option<bool>,
    #[serde(default)]
    is_active: Option<bool>,
}

fn paginate<T: serde::Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}

// ---------------------------------------------------------------------------
// Company handlers.
// ---------------------------------------------------------------------------

async fn list_companies(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<Company> = match db_pool(&ctx) {
            Some(pool) => {
                use crate::db::schema::companies::dsl as comp_dsl;
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

async fn create_company(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateCompanyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Company> = (|| {
        insert_company(&ctx, &body.name, body.slug.as_deref())
    })();
    match result {
        Ok(company) => json::success_response(company, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_company(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Company> = (|| {
        let id = id.into_inner();
        if let Some(pool) = db_pool(&ctx) {
            use crate::db::schema::companies::dsl as comp_dsl;
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

async fn update_company(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    body: web::Json<UpdateCompanyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Company> = (|| {
        let id = id.into_inner();
        let mut company = companies_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Company not found"))?;
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
            if companies_store()
                .iter()
                .any(|entry| entry.value().slug == slug && *entry.key() != id)
            {
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
            use crate::db::schema::companies::dsl as comp_dsl;
            #[derive(AsChangeset)]
            #[diesel(table_name = crate::db::schema::companies)]
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
                    name: body.name.clone(),
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

async fn delete_company(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        if let Some(pool) = db_pool(&ctx) {
            use crate::db::schema::companies::dsl as comp_dsl;
            use crate::db::schema::company_sites::dsl as site_dsl;
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
        let removed = companies_store().remove(&id).is_some();
        if removed {
            Ok(serde_json::json!({ "deleted": true, "id": id }))
        } else {
            Err(UnifiedError::not_found("Company not found"))
        }
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Site handlers.
// ---------------------------------------------------------------------------

async fn list_sites(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<CompanySite> = match db_pool(&ctx) {
            Some(pool) => {
                use crate::db::schema::company_sites::dsl as site_dsl;
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

async fn create_site(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateSiteRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
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
        let duplicate = sites_store()
            .iter()
            .any(|entry| entry.value().domain == domain);
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
            use crate::db::schema::company_sites::dsl as site_dsl;
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
            demote_other_primaries(&ctx, site.company_id, site.id);
        }
        Ok(site)
    })();
    match result {
        Ok(site) => json::success_response(site, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_site(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CompanySite> = (|| {
        let id = id.into_inner();
        if let Some(pool) = db_pool(&ctx) {
            use crate::db::schema::company_sites::dsl as site_dsl;
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

async fn update_site(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    body: web::Json<UpdateSiteRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CompanySite> = (|| {
        let id = id.into_inner();
        let mut site = sites_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Company site not found"))?;
        // The site domain is immutable after creation.
        if let Some(domain) = &body.domain {
            let candidate = domain.trim().to_lowercase();
            if candidate != site.domain {
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
            use crate::db::schema::company_sites::dsl as site_dsl;
            #[derive(AsChangeset)]
            #[diesel(table_name = crate::db::schema::company_sites)]
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
                    name: body.name.clone(),
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
            demote_other_primaries(&ctx, site.company_id, site.id);
        }
        Ok(site)
    })();
    json::respond(result, debug)
}

async fn delete_site(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        if let Some(pool) = db_pool(&ctx) {
            use crate::db::schema::company_sites::dsl as site_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            diesel::delete(site_dsl::company_sites.filter(site_dsl::id.eq(id)))
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot delete company site: {error}"))
                })?;
        }
        let removed = sites_store().remove(&id).is_some();
        if removed {
            Ok(serde_json::json!({ "deleted": true, "id": id }))
        } else {
            Err(UnifiedError::not_found("Company site not found"))
        }
    })();
    json::respond(result, debug)
}

async fn list_company_sites(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        if !company_exists(&ctx, id) {
            return Err(UnifiedError::not_found("Company not found"));
        }
        let mut items: Vec<CompanySite> = match db_pool(&ctx) {
            Some(pool) => {
                use crate::db::schema::company_sites::dsl as site_dsl;
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

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct CompanyModule;

impl Module for CompanyModule {
    fn id(&self) -> &'static str {
        "company"
    }

    fn name(&self) -> &'static str {
        "Company Management"
    }

    fn description(&self) -> &'static str {
        "Multi-site, multi-company management: companies and their sites, with immutable site domains and exactly one primary site per company."
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        let _ = COMPANY_CTX.set(stash_ctx(ctx));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        bus.register("company.exists", "company", Arc::new(on_company_exists));
        bus.register(
            "company.create_requested",
            "company",
            Arc::new(on_company_create_requested),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/company/companies", owner: "company", description: "List companies" },
            RouteInfo { method: "POST", path: "/api/v1/company/companies", owner: "company", description: "Create company" },
            RouteInfo { method: "GET", path: "/api/v1/company/companies/{id}", owner: "company", description: "Get company" },
            RouteInfo { method: "PUT", path: "/api/v1/company/companies/{id}", owner: "company", description: "Update company" },
            RouteInfo { method: "DELETE", path: "/api/v1/company/companies/{id}", owner: "company", description: "Delete company" },
            RouteInfo { method: "GET", path: "/api/v1/company/companies/{id}/sites", owner: "company", description: "List company sites" },
            RouteInfo { method: "GET", path: "/api/v1/company/{id}/sites", owner: "company", description: "List company sites" },
            RouteInfo { method: "GET", path: "/api/v1/company/sites", owner: "company", description: "List sites" },
            RouteInfo { method: "POST", path: "/api/v1/company/sites", owner: "company", description: "Create site" },
            RouteInfo { method: "GET", path: "/api/v1/company/sites/{id}", owner: "company", description: "Get site" },
            RouteInfo { method: "PUT", path: "/api/v1/company/sites/{id}", owner: "company", description: "Update site" },
            RouteInfo { method: "DELETE", path: "/api/v1/company/sites/{id}", owner: "company", description: "Delete site" },
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/company")
                .route("/companies", web::get().to(list_companies))
                .route("/companies", web::post().to(create_company))
                .route("/companies/{id}", web::get().to(get_company))
                .route("/companies/{id}", web::put().to(update_company))
                .route("/companies/{id}", web::delete().to(delete_company))
                .route("/companies/{id}/sites", web::get().to(list_company_sites))
                .route("/{id}/sites", web::get().to(list_company_sites))
                .route("/sites", web::get().to(list_sites))
                .route("/sites", web::post().to(create_site))
                .route("/sites/{id}", web::get().to(get_site))
                .route("/sites/{id}", web::put().to(update_site))
                .route("/sites/{id}", web::delete().to(delete_site)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(CompanyModule));
}
