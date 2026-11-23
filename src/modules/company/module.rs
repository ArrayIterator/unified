// Company module — module definition and registration.
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
            RouteInfo { method: "GET", path: "/api/v1/company/companies", owner: "company", description: "List companies", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/company/companies", owner: "company", description: "Create company", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/company/companies/{id}", owner: "company", description: "Get company", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/company/companies/{id}", owner: "company", description: "Update company", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/company/companies/{id}", owner: "company", description: "Delete company", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/company/companies/{id}/sites", owner: "company", description: "List company sites", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/company/sites", owner: "company", description: "List sites", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/company/sites", owner: "company", description: "Create site", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/company/sites/{id}", owner: "company", description: "Get site", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/company/sites/{id}", owner: "company", description: "Update site", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/company/sites/{id}", owner: "company", description: "Delete site", auth: unified_core::auth::RouteAuth::Authenticated},
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_info_has_no_duplicate_sites_alias() {
        let module = CompanyModule;
        let routes = module.route_info();
        assert_eq!(routes.len(), 11);
        assert!(
            !routes
                .iter()
                .any(|route| route.path == "/api/v1/company/{id}/sites"),
            "the /{{id}}/sites alias must be gone"
        );
        assert!(routes
            .iter()
            .any(|route| route.path == "/api/v1/company/companies/{id}/sites"));
        let mut seen = std::collections::HashSet::new();
        for route in &routes {
            assert!(
                seen.insert((route.method, route.path)),
                "duplicate route {} {}",
                route.method,
                route.path
            );
        }
    }
}
