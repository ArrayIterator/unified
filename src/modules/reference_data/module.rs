// Reference Data -- module struct, Module impl and registration.
//
// A CORE module (the `Module` trait defaults `is_core` to true): the
// four lists are platform-shared foundation data, always available,
// and the module is never disabled. It has no state, no database
// tables and no write endpoints, so there is nothing to initialise
// and no migration. `requires()` is empty: the module only reads
// compiled-in lists owned by unified-core, i18n and settings, and
// none of those depend on it in return.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::modules::{Module, ModuleRegistry};
use actix_web::web;
use std::sync::Arc;

use super::handlers::*;

pub struct ReferenceDataModule;

impl Default for ReferenceDataModule {
    fn default() -> Self {
        Self
    }
}

impl Module for ReferenceDataModule {
    fn id(&self) -> &'static str {
        "reference_data"
    }

    fn name(&self) -> &'static str {
        "Reference Data"
    }

    fn description(&self) -> &'static str {
        "Platform-shared reference lists: countries with flags (ISO 3166-1), currencies (ISO 4217), timezones (IANA) and languages (ISO 639-1). Read-only; one single source per list."
    }

    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {
        Ok(())
    }

    fn route_info(&self) -> Vec<unified_core::modules::RouteInfo> {
        vec![
            unified_core::modules::RouteInfo {
                method: "GET",
                path: "/api/v1/reference-data",
                owner: "reference_data",
                description: "Reference data index",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            unified_core::modules::RouteInfo {
                method: "GET",
                path: "/api/v1/reference-data/countries",
                owner: "reference_data",
                description: "List countries with flags",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            unified_core::modules::RouteInfo {
                method: "GET",
                path: "/api/v1/reference-data/countries/{code}",
                owner: "reference_data",
                description: "Get country",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            unified_core::modules::RouteInfo {
                method: "GET",
                path: "/api/v1/reference-data/currencies",
                owner: "reference_data",
                description: "List currencies",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            unified_core::modules::RouteInfo {
                method: "GET",
                path: "/api/v1/reference-data/currencies/{code}",
                owner: "reference_data",
                description: "Get currency",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            unified_core::modules::RouteInfo {
                method: "GET",
                path: "/api/v1/reference-data/timezones",
                owner: "reference_data",
                description: "List timezones",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            unified_core::modules::RouteInfo {
                method: "GET",
                path: "/api/v1/reference-data/languages",
                owner: "reference_data",
                description: "List languages",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/reference-data")
                .route("", web::get().to(index))
                .route("/countries", web::get().to(list_countries))
                .route("/countries/{code}", web::get().to(get_country))
                .route("/currencies", web::get().to(list_currencies))
                .route("/currencies/{code}", web::get().to(get_currency))
                .route("/timezones", web::get().to(list_timezones))
                .route("/languages", web::get().to(list_languages)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ReferenceDataModule));
}
