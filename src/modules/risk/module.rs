// risk - module.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
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
use super::handlers_risks::*;
use super::handlers_plans::*;
use super::handlers_templates::*;
use super::handlers_instances::*;

// Module

pub struct RiskModule {
    pub(crate) state: Arc<RiskState>,
}

impl RiskModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RiskState::new()),
        }
    }
}

impl Default for RiskModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for RiskModule {
    fn id(&self) -> &'static str {
        "risk"
    }

    fn name(&self) -> &'static str {
        "Risk Mitigation"
    }

    fn description(&self) -> &'static str {
        "Risk register with server-computed likelihood x impact scores and bands, mitigation plans with tasks requested from the Projects module, threshold and review-date alerts, and compliance checklists."
    }

    fn requires(&self) -> &[&'static str] {
        &["notifications", "audit", "projects"]
    }

    fn available_as_product(&self) -> bool {
        false
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("risk", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_review_check_requested(state.clone(), ctx));
        bus.register("risk.review_check_requested", "risk", handler);
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/risk")
                // Literal segments first: they must win over `/{id}`.
                .route("/matrix", web::get().to(risk_matrix))
                .route("/review-check", web::post().to(review_check))
                .route("/alerts", web::get().to(list_alerts))
                .route("/risks", web::get().to(list_risks))
                .route("/risks", web::post().to(create_risk))
                .route("/risks/{id}", web::get().to(get_risk))
                .route("/risks/{id}", web::put().to(update_risk))
                .route("/risks/{id}", web::delete().to(delete_risk))
                .route("/risks/{id}/plans", web::get().to(list_plans_for_risk))
                .route("/risks/{id}/plans", web::post().to(create_plan))
                .route("/plans/{id}", web::get().to(get_plan))
                .route("/plans/{id}", web::put().to(update_plan))
                .route("/plans/{id}", web::delete().to(delete_plan))
                .route("/plans/{id}/tasks", web::get().to(list_tasks_for_plan))
                .route("/plans/{id}/tasks", web::post().to(create_task))
                .route("/tasks/{id}", web::put().to(update_task))
                .route("/tasks/{id}", web::delete().to(delete_task))
                .route("/checklist-templates", web::get().to(list_templates))
                .route("/checklist-templates", web::post().to(create_template))
                .route("/checklist-templates/{id}", web::get().to(get_template))
                .route("/checklist-templates/{id}", web::put().to(update_template))
                .route("/checklist-templates/{id}", web::delete().to(delete_template))
                .route(
                    "/checklist-templates/{id}/items",
                    web::get().to(list_template_items),
                )
                .route(
                    "/checklist-templates/{id}/items",
                    web::post().to(add_template_item),
                )
                .route(
                    "/checklist-templates/{template_id}/items/{item_id}",
                    web::delete().to(delete_template_item),
                )
                .route("/checklist-instances", web::get().to(list_instances))
                .route("/checklist-instances", web::post().to(create_instance))
                .route("/checklist-instances/{id}", web::get().to(get_instance))
                .route(
                    "/checklist-instances/{id}/items/{item_id}/complete",
                    web::post().to(complete_instance_item),
                )
                .route(
                    "/checklist-instances/{id}/items/{item_id}/reopen",
                    web::post().to(reopen_instance_item),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/risk/risks", owner: "risk", description: "List risks (paginated; filter by company_id, category, status, band, owner_id, min_score).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/risk/risks", owner: "risk", description: "Create a risk (score and band are computed server-side).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/risk/risks/{id}", owner: "risk", description: "Get one risk.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/risk/risks/{id}", owner: "risk", description: "Update a risk (score and band are recomputed server-side).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/risk/risks/{id}", owner: "risk", description: "Delete a risk with its plans, tasks, and alerts.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/risk/matrix", owner: "risk", description: "5x5 likelihood x impact matrix with counts and band thresholds.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/risk/risks/{id}/plans", owner: "risk", description: "List a risk's mitigation plans.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/risk/risks/{id}/plans", owner: "risk", description: "Create a mitigation plan for a risk.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/risk/plans/{id}", owner: "risk", description: "Get one mitigation plan.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/risk/plans/{id}", owner: "risk", description: "Update a mitigation plan.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/risk/plans/{id}", owner: "risk", description: "Delete a mitigation plan with its tasks.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/risk/plans/{id}/tasks", owner: "risk", description: "List a plan's mitigation tasks.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/risk/plans/{id}/tasks", owner: "risk", description: "Create a mitigation task and request its backing task from the Projects module.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/risk/tasks/{id}", owner: "risk", description: "Update a mitigation task.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/risk/tasks/{id}", owner: "risk", description: "Delete a mitigation task.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/risk/review-check", owner: "risk", description: "Run the review-date scan and raise review_overdue alerts.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/risk/alerts", owner: "risk", description: "List risk alerts (threshold crossings and overdue reviews).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/risk/checklist-templates", owner: "risk", description: "List compliance checklist templates.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/risk/checklist-templates", owner: "risk", description: "Create a compliance checklist template.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/risk/checklist-templates/{id}", owner: "risk", description: "Get one checklist template.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/risk/checklist-templates/{id}", owner: "risk", description: "Update a checklist template.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/risk/checklist-templates/{id}", owner: "risk", description: "Delete a checklist template (refused while instances exist).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/risk/checklist-templates/{id}/items", owner: "risk", description: "List a template's checklist items.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/risk/checklist-templates/{id}/items", owner: "risk", description: "Add an item to a checklist template.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/risk/checklist-templates/{template_id}/items/{item_id}", owner: "risk", description: "Delete a checklist template item.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/risk/checklist-instances", owner: "risk", description: "List checklist instances.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/risk/checklist-instances", owner: "risk", description: "Create a checklist instance from a template.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/risk/checklist-instances/{id}", owner: "risk", description: "Get a checklist instance with its items.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/risk/checklist-instances/{id}/items/{item_id}/complete", owner: "risk", description: "Complete a checklist item with evidence notes.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/risk/checklist-instances/{id}/items/{item_id}/reopen", owner: "risk", description: "Reopen a completed checklist item.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(RiskModule::new()));
}
