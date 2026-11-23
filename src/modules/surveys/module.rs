// surveys module — Module definition and registration.
//
// Split from the former single-file module `src/modules/surveys.rs`.
// The module root (`surveys.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine;
use chrono::{DateTime, Datelike, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::{ORDER_PAID, SEND_REQUESTED, STORE_ORDER_COMPLETED, TICKET_UPDATED};
use super::types::Survey;
use super::state::SurveysState;
use super::hooks::{on_order_completed, on_send_requested, on_ticket_updated};
use super::handlers_surveys::{archive_survey, create_survey, delete_survey, get_survey, list_surveys, publish_survey, update_survey};
use super::handlers_questions::{create_question, delete_question, get_question, list_questions, update_question};
use super::handlers_results::{get_response, list_responses, survey_results};
use super::handlers_distributions::{create_distribution, delete_distribution, get_distribution, list_distributions, send_distribution};
use super::handlers_public::{get_public_survey, submit_response};
use super::ai_capabilities::AI_CAPABILITIES;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct SurveysModule {
    pub(crate) state: SurveysState,
}

impl SurveysModule {
    pub fn new() -> Self {
        Self {
            state: SurveysState::default(),
        }
    }
}

impl Default for SurveysModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for SurveysModule {
    fn id(&self) -> &'static str {
        "surveys"
    }

    fn name(&self) -> &'static str {
        "Surveys / NPS / CSAT"
    }

    fn description(&self) -> &'static str {
        "Survey builder with versions (editing a published survey creates a new version; responses reference the version they answered), distribution triggers (ticket close, purchase, scheduled, manual) with tokenized response links, NPS and CSAT scoring computed on read with trends and breakdowns by agent, department, and product, and low-score follow-ups requested from the tasks module."
    }

    fn requires(&self) -> &[&'static str] {
        &["tickets", "store", "tasks", "notifications", "scheduler", "roles", "users"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["tickets", "store", "tasks", "notifications", "scheduler", "roles", "users"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("communication")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("surveys", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            TICKET_UPDATED,
            "surveys",
            Arc::new(move |ctx: &mut HookContext| on_ticket_updated(state.clone(), ctx)),
        );
        for event in [STORE_ORDER_COMPLETED, ORDER_PAID] {
            let state = self.state.clone();
            bus.register(
                event,
                "surveys",
                Arc::new(move |ctx: &mut HookContext| on_order_completed(state.clone(), ctx)),
            );
        }
        let state = self.state.clone();
        bus.register(
            SEND_REQUESTED,
            "surveys",
            Arc::new(move |ctx: &mut HookContext| on_send_requested(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/surveys")
                .app_data(state)
                .route("", web::get().to(list_surveys))
                .route("", web::post().to(create_survey))
                .route("/public/{token}", web::get().to(get_public_survey))
                .route("/public/{token}", web::post().to(submit_response))
                .route("/responses/{response_id}", web::get().to(get_response))
                .route("/distributions/{id}", web::get().to(get_distribution))
                .route("/distributions/{id}/send", web::post().to(send_distribution))
                .route("/distributions/{id}", web::delete().to(delete_distribution))
                .route("/{id}", web::get().to(get_survey))
                .route("/{id}", web::patch().to(update_survey))
                .route("/{id}", web::delete().to(delete_survey))
                .route("/{id}/publish", web::post().to(publish_survey))
                .route("/{id}/archive", web::post().to(archive_survey))
                .route("/{id}/questions", web::get().to(list_questions))
                .route("/{id}/questions", web::post().to(create_question))
                .route("/{id}/questions/{question_id}", web::get().to(get_question))
                .route("/{id}/questions/{question_id}", web::patch().to(update_question))
                .route("/{id}/questions/{question_id}", web::delete().to(delete_question))
                .route("/{id}/results", web::get().to(survey_results))
                .route("/{id}/responses", web::get().to(list_responses))
                .route("/{id}/distributions", web::get().to(list_distributions))
                .route("/{id}/distributions", web::post().to(create_distribution)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/surveys", owner: "surveys", description: "List surveys (paginated, filter by status, family_id, created_by; every version is a row).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/surveys", owner: "surveys", description: "Create a survey (draft, version 1).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/surveys/{id}", owner: "surveys", description: "Get one survey version with its questions.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/surveys/{id}", owner: "surveys", description: "Update a draft in place; on a published survey creates a new draft version and returns it.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/surveys/{id}", owner: "surveys", description: "Delete a draft survey version (with its questions and pending invitations).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/surveys/{id}/publish", owner: "surveys", description: "Publish a draft version (the previously published version of the same family is archived).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/surveys/{id}/archive", owner: "surveys", description: "Archive a survey version.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/surveys/{id}/questions", owner: "surveys", description: "List questions of a survey version (position order).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/surveys/{id}/questions", owner: "surveys", description: "Add a question to a draft survey (nps, csat, rating, text, multiple_choice).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/surveys/{id}/questions/{question_id}", owner: "surveys", description: "Get one question.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/surveys/{id}/questions/{question_id}", owner: "surveys", description: "Update a question of a draft survey.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/surveys/{id}/questions/{question_id}", owner: "surveys", description: "Delete a question of a draft survey (only when no answers reference it).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/surveys/{id}/results", owner: "surveys", description: "NPS + CSAT scores, per-question stats, breakdowns by agent / department / product, and daily/weekly trends.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/surveys/{id}/responses", owner: "surveys", description: "List responses of a survey version (paginated, filter by status, score band, agent_id, department_id, product_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/surveys/responses/{response_id}", owner: "surveys", description: "Get one response with its answers and follow-up link.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/surveys/{id}/distributions", owner: "surveys", description: "List distributions of a survey version.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/surveys/{id}/distributions", owner: "surveys", description: "Create a distribution (trigger manual, ticket_closed, purchase, or scheduled).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/surveys/distributions/{id}", owner: "surveys", description: "Get one distribution.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/surveys/distributions/{id}/send", owner: "surveys", description: "Send a distribution's pending invitations now (manual / scheduled).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/surveys/distributions/{id}", owner: "surveys", description: "Delete a pending distribution (with its pending invitations).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/surveys/public/{token}", owner: "surveys", description: "PUBLIC: the survey + questions for one response token (no auth; gated by the API Manager ApiAccessRule for this path).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/surveys/public/{token}", owner: "surveys", description: "PUBLIC: submit answers for one response token (no auth; gated by the API Manager ApiAccessRule for this path).", auth: unified_core::auth::RouteAuth::Public},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SurveysModule::new()));
}
