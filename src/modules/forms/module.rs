// forms — module.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::store::*;
use super::common::*;
use super::validation::*;
use super::submission::*;
use super::handlers_forms::*;
use super::handlers_fields::*;
use super::handlers_submissions::*;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct FormsModule {
    pub(crate) state: FormsState,
}

impl FormsModule {
    pub fn new() -> Self {
        Self {
            state: FormsState::default(),
        }
    }
}

impl Default for FormsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for FormsModule {
    fn id(&self) -> &'static str {
        "forms"
    }

    fn name(&self) -> &'static str {
        "Form Builder"
    }

    fn description(&self) -> &'static str {
        "Form builder: definitions with ordered fields, validation rules and conditional visibility, embed codes, a submissions inbox with spam protection, and submission mapping to CRM leads, support tickets, or tasks."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "crm", "tickets", "tasks", "storage"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "crm", "tickets", "tasks", "storage"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("forms", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, _bus: &HookBus) {}

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/forms")
                .app_data(state)
                .route("", web::get().to(list_forms))
                .route("", web::post().to(create_form))
                .route("/public/{slug}", web::get().to(get_public_form))
                .route("/public/{slug}/submit", web::post().to(submit_public_form))
                .route("/submissions/{submission_id}", web::get().to(get_submission))
                .route(
                    "/submissions/{submission_id}/read",
                    web::post().to(mark_submission_read),
                )
                .route(
                    "/submissions/{submission_id}/spam",
                    web::post().to(mark_submission_spam),
                )
                .route(
                    "/submissions/{submission_id}/remap",
                    web::post().to(remap_submission),
                )
                .route("/{id}", web::get().to(get_form))
                .route("/{id}", web::patch().to(update_form))
                .route("/{id}", web::delete().to(delete_form))
                .route("/{id}/embed", web::get().to(get_embed_code))
                .route("/{id}/fields", web::get().to(list_fields))
                .route("/{id}/fields", web::post().to(create_field))
                .route("/{id}/fields/reorder", web::post().to(reorder_fields))
                .route("/{id}/fields/{field_id}", web::get().to(get_field))
                .route("/{id}/fields/{field_id}", web::patch().to(update_field))
                .route("/{id}/fields/{field_id}", web::delete().to(delete_field))
                .route("/{id}/submissions", web::get().to(list_submissions))
                .route("/{id}/submissions/export", web::get().to(export_submissions)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/forms", owner: "forms", description: "List forms (paginated, filter by status, company_id, search).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/forms", owner: "forms", description: "Create a form with its fields (ordered, validated, conditional visibility).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/forms/{id}", owner: "forms", description: "Get one form with its fields.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/forms/{id}", owner: "forms", description: "Update a form (title, status, labels, honeypot field, mapping).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/forms/{id}", owner: "forms", description: "Delete a form with its fields and submissions.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/forms/{id}/embed", owner: "forms", description: "Embed code for a form (render + submit URLs, script and iframe snippets).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/forms/{id}/fields", owner: "forms", description: "List fields of a form (position order).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/forms/{id}/fields", owner: "forms", description: "Add a field to a form (validated definition).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/forms/{id}/fields/{field_id}", owner: "forms", description: "Get one field.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/forms/{id}/fields/{field_id}", owner: "forms", description: "Update a field definition (rules re-validated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/forms/{id}/fields/{field_id}", owner: "forms", description: "Delete a field with its submitted values.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/forms/{id}/fields/reorder", owner: "forms", description: "Reorder fields by id list (drag-drop order).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/forms/{id}/submissions", owner: "forms", description: "Submissions inbox (paginated, filter by status, mapping_status, search).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/forms/{id}/submissions/export", owner: "forms", description: "Export submissions as CSV or JSON (?format=csv|json).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/forms/submissions/{submission_id}", owner: "forms", description: "Get one submission with its values.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/forms/submissions/{submission_id}/read", owner: "forms", description: "Mark a submission read.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/forms/submissions/{submission_id}/spam", owner: "forms", description: "Mark a submission spam.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/forms/submissions/{submission_id}/remap", owner: "forms", description: "Retry the submission mapping (only when the mapping failed).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/forms/public/{slug}", owner: "forms", description: "PUBLIC: render a published form definition as JSON for embedding (no auth; gated by the API Manager ApiAccessRule).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/forms/public/{slug}/submit", owner: "forms", description: "PUBLIC: submit a published form (honeypot + rate limits + server-side validation + mapping; no auth; gated by the API Manager ApiAccessRule).", auth: unified_core::auth::RouteAuth::Public},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(FormsModule::new()));
}
