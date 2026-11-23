// crm — module.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::store::*;
use super::common::*;
use super::handlers_contacts::*;
use super::handlers_leads::*;
use super::handlers_activities::*;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct CrmModule {
    pub(crate) state: Arc<CrmState>,
}

impl CrmModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(CrmState::new()),
        }
    }
}

impl Default for CrmModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for CrmModule {
    fn id(&self) -> &'static str {
        "crm"
    }

    fn name(&self) -> &'static str {
        "CRM"
    }

    fn description(&self) -> &'static str {
        "CRM contacts and leads: pipeline, conversion and activities."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["company", "users"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("crm", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Completed appointments arrive as `crm.activity.requested`
        // and are recorded as activities on the matching contact/lead.
        let state = self.state.clone();
        bus.register(
            "crm.activity.requested",
            "crm",
            Arc::new(move |ctx: &mut HookContext| {
                on_crm_activity_requested(state.clone(), ctx)
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/crm/contacts", owner: "crm", description: "List contacts", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/crm/contacts", owner: "crm", description: "Create contact", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/crm/contacts/{id}", owner: "crm", description: "Get contact", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/crm/contacts/{id}", owner: "crm", description: "Update contact", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/crm/contacts/{id}", owner: "crm", description: "Delete contact", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/crm/leads", owner: "crm", description: "List leads", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/crm/leads", owner: "crm", description: "Create lead", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/crm/leads/{id}", owner: "crm", description: "Get lead", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/crm/leads/{id}", owner: "crm", description: "Update lead", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/crm/leads/{id}", owner: "crm", description: "Delete lead", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/crm/leads/{id}/convert", owner: "crm", description: "Convert lead", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/crm/pipeline", owner: "crm", description: "Get pipeline", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/crm/{type}/{id}/activities", owner: "crm", description: "List activities", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/crm/{type}/{id}/activities", owner: "crm", description: "Add activity", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/crm")
                .route("/contacts", web::get().to(list_contacts))
                .route("/contacts", web::post().to(create_contact))
                .route("/contacts/{id}", web::get().to(get_contact))
                .route("/contacts/{id}", web::put().to(update_contact))
                .route("/contacts/{id}", web::delete().to(delete_contact))
                .route("/leads", web::get().to(list_leads))
                .route("/leads", web::post().to(create_lead))
                .route("/leads/{id}", web::get().to(get_lead))
                .route("/leads/{id}", web::put().to(update_lead))
                .route("/leads/{id}", web::delete().to(delete_lead))
                .route("/leads/{id}/convert", web::post().to(convert_lead))
                .route("/pipeline", web::get().to(get_pipeline))
                .route("/{type}/{id}/activities", web::get().to(list_activities))
                .route("/{type}/{id}/activities", web::post().to(add_activity)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(CrmModule::new()));
}
