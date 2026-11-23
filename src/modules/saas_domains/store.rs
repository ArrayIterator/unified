//! SaaS Domains - in-memory stores.
//!
//! Part of the `saas_domains` module split; see `src/modules/saas_domains.rs`.

use unified_core::context::AppContext;
use unified_core::pagination::PageResult;
use unified_core::pagination::PaginationParams;
use dashmap::DashMap;
use serde::Serialize;
use std::sync::OnceLock;
use std::sync::RwLock;
use uuid::Uuid;

use super::settings::SaasDomainsSettings;
use super::types::DomainAlias;
use super::types::DomainReservation;
use super::types::ProductSubdomain;
use super::types::SubdomainAllocation;

// ---------------------------------------------------------------------------
// Stores (authoritative at runtime; migration tables are the
// persistence targets for the integrator's Diesel layer).
// ---------------------------------------------------------------------------

static RESERVATIONS: OnceLock<DashMap<Uuid, DomainReservation>> = OnceLock::new();

static ALIASES: OnceLock<DashMap<Uuid, DomainAlias>> = OnceLock::new();

static ALLOCATIONS: OnceLock<DashMap<Uuid, SubdomainAllocation>> = OnceLock::new();

static PRODUCT_SUBDOMAINS: OnceLock<DashMap<Uuid, ProductSubdomain>> = OnceLock::new();

static SETTINGS: OnceLock<RwLock<SaasDomainsSettings>> = OnceLock::new();

/// Main host resolved from the listen addresses by `resolve_host`
/// when the `main_host` setting is empty, persisted here so the
/// hook handler (which has no `AppContext` to read the listen
/// addresses from) resolves the same control-plane host instead of
/// falling back to `localhost` on its own. Empty until the first
/// `resolve_host` call computes it.
static RESOLVED_MAIN_HOST: OnceLock<RwLock<String>> = OnceLock::new();

pub(crate) fn resolved_main_host_store() -> &'static RwLock<String> {
    RESOLVED_MAIN_HOST.get_or_init(|| RwLock::new(String::new()))
}

pub(crate) fn resolved_main_host() -> String {
    resolved_main_host_store()
        .read()
        .expect("saas_domains resolved main host lock poisoned")
        .clone()
}

pub(crate) fn set_resolved_main_host(host: &str) {
    let mut slot = resolved_main_host_store()
        .write()
        .expect("saas_domains resolved main host lock poisoned");
    if slot.as_str() != host {
        *slot = host.to_string();
    }
}

pub(crate) fn reservations_store() -> &'static DashMap<Uuid, DomainReservation> {
    RESERVATIONS.get_or_init(DashMap::new)
}

pub(crate) fn aliases_store() -> &'static DashMap<Uuid, DomainAlias> {
    ALIASES.get_or_init(DashMap::new)
}

pub(crate) fn allocations_store() -> &'static DashMap<Uuid, SubdomainAllocation> {
    ALLOCATIONS.get_or_init(DashMap::new)
}

pub(crate) fn product_subdomains_store() -> &'static DashMap<Uuid, ProductSubdomain> {
    PRODUCT_SUBDOMAINS.get_or_init(DashMap::new)
}

pub(crate) fn settings_store() -> &'static RwLock<SaasDomainsSettings> {
    SETTINGS.get_or_init(|| RwLock::new(SaasDomainsSettings::default()))
}

pub(crate) fn settings() -> SaasDomainsSettings {
    settings_store()
        .read()
        .expect("saas_domains settings lock poisoned")
        .clone()
}

pub(crate) fn db_pool(ctx: &AppContext) -> Option<unified_core::db::DbPool> {
    ctx.db.read().expect("db lock poisoned").clone()
}

pub(crate) fn paginate<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
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
