//! email templates - module.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::HookBus;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::web;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use super::render::*;
use super::types::*;
use super::ai_capabilities::AI_CAPABILITIES;
use super::handlers::{create_template, delete_template, get_template, list_by_category, list_templates, list_variables, list_versions, preview_template, restore_version, test_send_template, update_template};

pub struct EmailTemplatesModule {
    state: Arc<EmailTemplateState>,
}

impl EmailTemplatesModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(EmailTemplateState::new()),
        }
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(EmailTemplatesModule::new()));
}

impl Module for EmailTemplatesModule {
    fn id(&self) -> &'static str {
        "email-templates"
    }

    fn name(&self) -> &'static str {
        "Email Template Builder"
    }

    fn description(&self) -> &'static str {
        "GrapeJS email templates with versioning, placeholder variables, preview rendering, and test sending through the Mail Manager."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["mail"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("email-templates", AI_CAPABILITIES);
        self.state.debug.store(ctx.is_debug(), Ordering::SeqCst);
        self.state.set_hooks(ctx.hooks.clone());
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Rendering service used by the Mail Manager (and any other module)
        // when a template_id is supplied for sending.
        let state = self.state.clone();
        bus.register(
            EVENT_TEMPLATE_RENDER_REQUESTED,
            "email-templates",
            Arc::new(move |ctx| {
                let template_id = ctx
                    .get("template_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let variables: HashMap<String, Value> = ctx
                    .get("variables")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                match state.get(&template_id) {
                    Some(template) => {
                        let (rendered_html, subject, missing) =
                            EmailTemplateState::render(&template, &variables);
                        ctx.add_result(json!({
                            "template_id": template_id,
                            "rendered_html": rendered_html,
                            "subject": subject,
                            "missing_variables": missing
                        }));
                    }
                    None => {
                        ctx.add_result(json!({
                            "error": format!("Email template not found: {template_id}")
                        }));
                    }
                }
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/email-templates/by-category/{category}", owner: "email-templates", description: "List by category", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/email-templates", owner: "email-templates", description: "List templates", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/email-templates", owner: "email-templates", description: "Create template", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/email-templates/{id}", owner: "email-templates", description: "Get template", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/email-templates/{id}", owner: "email-templates", description: "Update template", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/email-templates/{id}", owner: "email-templates", description: "Delete template", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/email-templates/{id}/versions", owner: "email-templates", description: "List versions", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/email-templates/{id}/restore/{version}", owner: "email-templates", description: "Restore version", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/email-templates/{id}/variables", owner: "email-templates", description: "List variables", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/email-templates/{id}/preview", owner: "email-templates", description: "Preview template", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/email-templates/{id}/test-send", owner: "email-templates", description: "Test send template", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        let state = self.state.clone();
        cfg.service(
            web::scope("/api/v1/email-templates")
                .app_data(web::Data::new(state))
                // Static segment first so `/by-category/{category}` is
                // never captured by the `/{id}` routes below.
                .route("/by-category/{category}", web::get().to(list_by_category))
                .route("", web::get().to(list_templates))
                .route("", web::post().to(create_template))
                .route("/{id}", web::get().to(get_template))
                .route("/{id}", web::put().to(update_template))
                .route("/{id}", web::delete().to(delete_template))
                .route("/{id}/versions", web::get().to(list_versions))
                .route("/{id}/restore/{version}", web::post().to(restore_version))
                .route("/{id}/variables", web::post().to(list_variables))
                .route("/{id}/preview", web::post().to(preview_template))
                .route("/{id}/test-send", web::post().to(test_send_template)),
        );
    }
}
