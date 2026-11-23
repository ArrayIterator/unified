// i18n -- Module struct, Module impl and registration.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

use super::handlers::*;
use super::state::*;
use super::ai_capabilities::AI_CAPABILITIES;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct I18nModule {
    state: I18nState,
}

impl I18nModule {
    pub fn new() -> Self {
        Self {
            state: I18nState::default(),
        }
    }
}

impl Default for I18nModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for I18nModule {
    fn id(&self) -> &'static str {
        "i18n"
    }

    fn name(&self) -> &'static str {
        "Translations"
    }

    fn description(&self) -> &'static str {
        "Language catalogue and key/value translations persisted to JSON files."
    }

    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("i18n", AI_CAPABILITIES);
        // Load persisted overrides; a failure here must not stop startup,
        // but a corrupt file is reported instead of silently ignored.
        self.state.load_from_disk()
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/i18n/languages",
                owner: "i18n",
                description: "List languages",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/i18n/languages/negotiate",
                owner: "i18n",
                description: "Negotiate language handler",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/i18n/currencies",
                owner: "i18n",
                description: "List currencies",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/i18n/currencies/{code}",
                owner: "i18n",
                description: "Get currency",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/i18n/translations",
                owner: "i18n",
                description: "List translations",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/i18n/translations",
                owner: "i18n",
                description: "Upsert translation",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/i18n/translate",
                owner: "i18n",
                description: "Translate",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/i18n/translate-plural",
                owner: "i18n",
                description: "Translate plural handler",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/i18n/catalog/po",
                owner: "i18n",
                description: "Load po catalog handler",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/i18n/translations/{lang}/{key}",
                owner: "i18n",
                description: "Delete translation",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/i18n")
                .app_data(state)
                .route("/languages", web::get().to(list_languages))
                .route(
                    "/languages/negotiate",
                    web::get().to(negotiate_language_handler),
                )
                .route("/currencies", web::get().to(list_currencies))
                .route("/currencies/{code}", web::get().to(get_currency))
                .route("/translations", web::get().to(list_translations))
                .route("/translations", web::post().to(upsert_translation))
                .route("/translate", web::post().to(translate))
                .route(
                    "/translate-plural",
                    web::post().to(translate_plural_handler),
                )
                .route("/catalog/po", web::post().to(load_po_catalog_handler))
                .route(
                    "/translations/{lang}/{key}",
                    web::delete().to(delete_translation),
                ),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(I18nModule::new()));
}
