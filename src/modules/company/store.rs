// Company module - in-memory stores, company/site lookups, and hook handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::{Company, CompanySite, NewCompany, NewCompanySite};
use crate::modules::network::DnsVerifier;
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use uuid::Uuid;
use super::*;

// In-memory stores (used when no database pool is attached).

pub(crate) static COMPANIES: OnceLock<DashMap<Uuid, Company>> = OnceLock::new();

pub(crate) static SITES: OnceLock<DashMap<Uuid, CompanySite>> = OnceLock::new();

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
        use unified_core::db::schema::companies::dsl as comp_dsl;
        if let Ok(mut conn) = pool.get() {
            if let Ok(Some(_)) = comp_dsl::companies
                .filter(comp_dsl::id.eq(id))
                .select(comp_dsl::id)
                .first::<Uuid>(&mut conn)
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
        use unified_core::db::schema::company_sites::dsl as site_dsl;
        if let Ok(mut conn) = pool.get() {
            if let Ok(Some(_)) = site_dsl::company_sites
                .filter(site_dsl::id.eq(id))
                .select(site_dsl::id)
                .first::<Uuid>(&mut conn)
                .optional()
            {
                return true;
            }
        }
    }
    false
}

pub(crate) fn db_pool(ctx: &AppContext) -> Option<unified_core::db::DbPool> {
    ctx.db.read().expect("db lock poisoned").clone()
}

// Hook context: `init` stashes the shared application context so the
// synchronous hook handlers below can use the database. Hook handlers
// read it lazily, so registration order (hooks before `init`) does not
// matter. The stashed context shares the live pool/config locks; only
// the uptime and pause flags are snapshots.

pub(crate) static COMPANY_CTX: OnceLock<Arc<AppContext>> = OnceLock::new();

pub(crate) fn stash_ctx(ctx: &AppContext) -> Arc<AppContext> {
    Arc::new(AppContext {
        config: ctx.config.clone(),
        hooks: ctx.hooks.clone(),
        modules: ctx.modules.clone(),
        cache: ctx.cache.clone(),
        geo: ctx.geo.clone(),
        db: ctx.db.clone(),
        storage_registry: ctx.storage_registry.clone(),
        routes: ctx.routes.clone(),
        tls: ctx.tls.clone(),
        start_time: std::time::Instant::now(),
        paused: std::sync::atomic::AtomicBool::new(ctx.is_paused()),
        maintenance: std::sync::atomic::AtomicBool::new(ctx.is_maintenance()),
    })
}

pub(crate) fn hook_ctx() -> Option<Arc<AppContext>> {
    COMPANY_CTX.get().cloned()
}

/// True when a company with this slug exists (in-memory or
/// database), optionally ignoring one id (the row being updated).
/// The database is consulted as well as the store, so a DB-only row
/// after a restart still conflicts instead of duplicating.
pub(crate) fn company_slug_exists(
    ctx: &AppContext,
    slug: &str,
    exclude: Option<Uuid>,
) -> UnifiedResult<bool> {
    if companies_store()
        .iter()
        .any(|entry| entry.value().slug == slug && Some(*entry.key()) != exclude)
    {
        return Ok(true);
    }
    if let Some(pool) = db_pool(ctx) {
        use unified_core::db::schema::companies::dsl as comp_dsl;
        let mut conn = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot get database connection: {error}"))
        })?;
        let found = if let Some(excluded) = exclude {
            comp_dsl::companies
                .filter(comp_dsl::slug.eq(slug))
                .filter(comp_dsl::id.ne(excluded))
                .select(comp_dsl::id)
                .first::<Uuid>(&mut conn)
                .optional()
        } else {
            comp_dsl::companies
                .filter(comp_dsl::slug.eq(slug))
                .select(comp_dsl::id)
                .first::<Uuid>(&mut conn)
                .optional()
        }
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot check company slug: {error}"))
        })?
        .is_some();
        return Ok(found);
    }
    Ok(false)
}

/// True when a site with this domain exists (in-memory or database).
/// The database is consulted as well as the store, so a DB-only row
/// after a restart still conflicts instead of duplicating.
pub(crate) fn site_domain_exists(ctx: &AppContext, domain: &str) -> UnifiedResult<bool> {
    if sites_store()
        .iter()
        .any(|entry| entry.value().domain == domain)
    {
        return Ok(true);
    }
    if let Some(pool) = db_pool(ctx) {
        use unified_core::db::schema::company_sites::dsl as site_dsl;
        let mut conn = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot get database connection: {error}"))
        })?;
        let found = site_dsl::company_sites
            .filter(site_dsl::domain.eq(domain))
            .select(site_dsl::id)
            .first::<Uuid>(&mut conn)
            .optional()
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot check site domain: {error}"))
            })?
            .is_some();
        return Ok(found);
    }
    Ok(false)
}

/// Create and store one company. Shared by the HTTP create handler and
/// the `company.create_requested` hook handler (CRM lead conversion).
/// The slug is derived from the name when none is supplied; a duplicate
/// slug is a conflict.
pub(crate) fn insert_company(
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
    let duplicate = company_slug_exists(ctx, &slug, None)?;
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
        use unified_core::db::schema::companies::dsl as comp_dsl;
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
pub(crate) fn on_company_exists(ctx: &mut HookContext) {
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
pub(crate) fn on_company_create_requested(ctx: &mut HookContext) {
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

pub(crate) fn slugify(name: &str) -> String {
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
/// A database failure is propagated to the caller instead of swallowed:
/// a failed demotion would otherwise leave two primary sites with no
/// error surfaced.
pub(crate) fn demote_other_primaries(
    ctx: &AppContext,
    company_id: Uuid,
    keep_id: Uuid,
) -> UnifiedResult<()> {
    for mut entry in sites_store().iter_mut() {
        if entry.value().company_id == company_id && entry.key() != &keep_id && entry.is_primary {
            entry.is_primary = false;
            entry.updated_at = Utc::now();
        }
    }
    if let Some(pool) = db_pool(ctx) {
        use unified_core::db::schema::company_sites::dsl as site_dsl;
        #[derive(AsChangeset)]
        #[diesel(table_name = unified_core::db::schema::company_sites)]
        struct PrimaryChange {
            is_primary: bool,
        }
        let mut conn = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot get database connection: {error}"))
        })?;
        diesel::update(
            site_dsl::company_sites
                .filter(site_dsl::company_id.eq(company_id))
                .filter(site_dsl::id.ne(keep_id)),
        )
        .set(PrimaryChange { is_primary: false })
        .execute(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!(
                "Cannot demote other primary sites: {error}"
            ))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_lowercases_and_uses_dashes() {
        assert_eq!(slugify("Our Cooperative"), "our-cooperative");
        assert_eq!(slugify("Branch 42"), "branch-42");
    }

    #[test]
    fn slugify_collapses_repeated_separators() {
        assert_eq!(slugify("A  --  B"), "a-b");
        assert_eq!(slugify("  padded name  "), "padded-name");
    }

    #[test]
    fn slugify_drops_non_ascii_and_punctuation() {
        assert_eq!(slugify("Café & Bar!"), "caf-bar");
        assert_eq!(slugify("!!!"), "");
        assert_eq!(slugify(""), "");
    }

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
    fn company_slug_exists_checks_the_store() {
        let ctx = test_ctx();
        let slug = format!("slug-{}", Uuid::new_v4().simple());
        assert!(!company_slug_exists(&ctx, &slug, None).unwrap());
        let id = Uuid::new_v4();
        let now = Utc::now();
        companies_store().insert(
            id,
            Company {
                id,
                name: "X".to_string(),
                slug: slug.clone(),
                is_active: true,
                created_at: now,
                updated_at: now,
            },
        );
        assert!(company_slug_exists(&ctx, &slug, None).unwrap());
        assert!(!company_slug_exists(&ctx, &slug, Some(id)).unwrap());
        assert!(company_slug_exists(&ctx, &slug, Some(Uuid::new_v4())).unwrap());
        companies_store().remove(&id);
    }

    #[test]
    fn site_domain_exists_checks_the_store() {
        let ctx = test_ctx();
        let domain = format!("{}.example.invalid", Uuid::new_v4().simple());
        assert!(!site_domain_exists(&ctx, &domain).unwrap());
        let id = Uuid::new_v4();
        let now = Utc::now();
        sites_store().insert(
            id,
            CompanySite {
                id,
                company_id: Uuid::new_v4(),
                name: "Main".to_string(),
                domain: domain.clone(),
                is_primary: true,
                is_active: true,
                created_at: now,
                updated_at: now,
            },
        );
        assert!(site_domain_exists(&ctx, &domain).unwrap());
        sites_store().remove(&id);
        assert!(!site_domain_exists(&ctx, &domain).unwrap());
    }

    #[test]
    fn insert_company_rejects_a_duplicate_slug() {
        let ctx = test_ctx();
        let name = format!("Acme {}", Uuid::new_v4().simple());
        let first = insert_company(&ctx, &name, None).unwrap();
        let error = insert_company(&ctx, &name, None).unwrap_err();
        assert_eq!(error.code, 409);
        companies_store().remove(&first.id);
    }

    #[test]
    fn demote_other_primaries_does_not_swallow_errors() {
        let ctx = test_ctx();
        let company_id = Uuid::new_v4();
        let now = Utc::now();
        let keep = Uuid::new_v4();
        let other = Uuid::new_v4();
        for id in [keep, other] {
            sites_store().insert(
                id,
                CompanySite {
                    id,
                    company_id,
                    name: "Site".to_string(),
                    domain: format!("{}.example.invalid", id.simple()),
                    is_primary: true,
                    is_active: true,
                    created_at: now,
                    updated_at: now,
                },
            );
        }
        demote_other_primaries(&ctx, company_id, keep).unwrap();
        assert!(sites_store().get(&keep).unwrap().is_primary);
        assert!(!sites_store().get(&other).unwrap().is_primary);
        sites_store().remove(&keep);
        sites_store().remove(&other);
    }
}
