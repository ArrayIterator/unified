// supervision module -- Module definition, registration, and route table.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

use super::*;

// Module

pub struct SupervisionModule {
    pub(crate) state: SupervisionState,
}

impl SupervisionModule {
    pub fn new() -> Self {
        Self {
            state: SupervisionState::default(),
        }
    }
}

impl Default for SupervisionModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for SupervisionModule {
    fn id(&self) -> &'static str {
        "supervision"
    }

    fn name(&self) -> &'static str {
        "Project Supervision"
    }

    fn description(&self) -> &'static str {
        "Site supervision for projects: supervisor assignments, inspection templates and checklists, conducted inspections with verdicts and signatures, progress-report snapshots with an approval workflow, a defect log with photo evidence, approval chains with a full audit trail, and daily site logs. Projects, tasks, and milestones stay owned by the Project Management module and are linked by id through hook queries only."
    }

    fn requires(&self) -> &[&'static str] {
        &["projects", "roles", "storage", "notifications"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["projects", "roles", "storage", "notifications"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("supervision", AI_CAPABILITIES);
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
            "project.created",
            "supervision",
            Arc::new(move |ctx: &mut HookContext| {
                on_project_changed(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "project.updated",
            "supervision",
            Arc::new(move |ctx: &mut HookContext| {
                on_project_changed(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "project.archived",
            "supervision",
            Arc::new(move |ctx: &mut HookContext| {
                on_project_archived(state.clone(), ctx)
            }),
        );
        for event in [
            "project.task.created",
            "project.task.assigned",
            "project.task.completed",
        ] {
            let state = self.state.clone();
            bus.register(
                event,
                "supervision",
                Arc::new(move |ctx: &mut HookContext| {
                    on_project_task_event(state.clone(), ctx)
                }),
            );
        }
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/supervision")
                .app_data(state)
                .route("/dashboard", web::get().to(project_dashboard))
                .route("/assignments", web::get().to(list_assignments))
                .route("/assignments", web::post().to(create_assignment))
                .route("/assignments/{id}", web::get().to(get_assignment))
                .route("/assignments/{id}", web::patch().to(update_assignment))
                .route("/assignments/{id}", web::delete().to(delete_assignment))
                .route("/templates", web::get().to(list_templates))
                .route("/templates", web::post().to(create_template))
                .route("/templates/{id}", web::get().to(get_template))
                .route("/templates/{id}", web::patch().to(update_template))
                .route("/templates/{id}", web::delete().to(delete_template))
                .route("/inspections", web::get().to(list_inspections))
                .route("/inspections", web::post().to(create_inspection))
                .route("/inspections/{id}", web::get().to(get_inspection))
                .route("/inspections/{id}", web::patch().to(update_inspection))
                .route("/inspections/{id}/submit", web::post().to(submit_inspection))
                .route("/inspections/{id}/complete", web::post().to(complete_inspection))
                .route("/inspections/{id}/approve", web::post().to(approve_inspection))
                .route("/inspections/{id}/return", web::post().to(return_inspection))
                .route(
                    "/inspections/{id}/items/{item_id}/result",
                    web::post().to(record_item_result),
                )
                .route(
                    "/inspections/{id}/items/{item_id}/follow-up",
                    web::post().to(follow_up_item),
                )
                .route("/progress-reports", web::get().to(list_progress_reports))
                .route("/progress-reports", web::post().to(create_progress_report))
                .route("/progress-reports/{id}", web::get().to(get_progress_report))
                .route(
                    "/progress-reports/{id}",
                    web::patch().to(update_progress_report),
                )
                .route(
                    "/progress-reports/{id}/submit",
                    web::post().to(submit_progress_report),
                )
                .route(
                    "/progress-reports/{id}/approve",
                    web::post().to(approve_progress_report),
                )
                .route(
                    "/progress-reports/{id}/return",
                    web::post().to(return_progress_report),
                )
                .route("/defects", web::get().to(list_defects))
                .route("/defects", web::post().to(create_defect))
                .route("/defects/{id}", web::get().to(get_defect))
                .route("/defects/{id}", web::patch().to(update_defect))
                .route("/defects/{id}/follow-up", web::post().to(follow_up_defect))
                .route("/defects/{id}/verify", web::post().to(verify_defect))
                .route("/approvals", web::get().to(list_approvals))
                .route("/daily-logs", web::get().to(list_daily_logs))
                .route("/daily-logs", web::post().to(create_daily_log))
                .route("/daily-logs/{id}", web::get().to(get_daily_log))
                .route("/daily-logs/{id}", web::patch().to(update_daily_log))
                .route("/daily-logs/{id}/sign-off", web::post().to(sign_off_daily_log))
                .route("/photos", web::get().to(list_photos))
                .route("/photos", web::post().to(create_photo))
                .route("/photos/{id}", web::get().to(get_photo))
                .route("/photos/{id}", web::delete().to(delete_photo)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/supervision/dashboard", owner: "supervision", description: "Per-project supervision summary (open defects, overdue inspections, progress variance) computed on read; requires project_id.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/supervision/assignments", owner: "supervision", description: "List supervision assignments (paginated, filter by project_id, user_id, role, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/supervision/assignments", owner: "supervision", description: "Assign a supervisor / inspector / approver to a project (project validated via project.get_requested).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/supervision/assignments/{id}", owner: "supervision", description: "Get one supervision assignment.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/supervision/assignments/{id}", owner: "supervision", description: "Update a supervision assignment (role, status, period, notes).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/supervision/assignments/{id}", owner: "supervision", description: "Delete a supervision assignment.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/supervision/templates", owner: "supervision", description: "List inspection templates (paginated, filter by project_id, category).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/supervision/templates", owner: "supervision", description: "Create an inspection template with checklist items.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/supervision/templates/{id}", owner: "supervision", description: "Get one inspection template.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/supervision/templates/{id}", owner: "supervision", description: "Update an inspection template (items are replaced wholesale).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/supervision/templates/{id}", owner: "supervision", description: "Delete an inspection template with no inspections created from it.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/supervision/inspections", owner: "supervision", description: "List inspections (paginated, filter by project_id, template_id, status, inspector_id, overdue).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/supervision/inspections", owner: "supervision", description: "Create an inspection, optionally from a template (template items copied as pending results).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/supervision/inspections/{id}", owner: "supervision", description: "Get one inspection with its item results.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/supervision/inspections/{id}", owner: "supervision", description: "Update a scheduled, in-progress, or returned inspection (send status \"cancelled\" to cancel it).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/supervision/inspections/{id}/submit", owner: "supervision", description: "Submit an inspection for approval (all items resulted, verdict recorded).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/supervision/inspections/{id}/complete", owner: "supervision", description: "Record the overall verdict and the inspector signature.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/supervision/inspections/{id}/approve", owner: "supervision", description: "Approve a submitted inspection (records an approval row).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/supervision/inspections/{id}/return", owner: "supervision", description: "Return a submitted inspection for rework (records an approval row).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/supervision/inspections/{id}/items/{item_id}/result", owner: "supervision", description: "Record pass / fail / na plus notes for one checklist item.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/supervision/inspections/{id}/items/{item_id}/follow-up", owner: "supervision", description: "Request a follow-up task in the projects module for a failed item.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/supervision/progress-reports", owner: "supervision", description: "List progress reports (paginated, filter by project_id, milestone_id, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/supervision/progress-reports", owner: "supervision", description: "Create a progress report snapshot (reported vs planned; project percent from project.progress_requested, source recorded).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/supervision/progress-reports/{id}", owner: "supervision", description: "Get one progress report with its derived variance.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/supervision/progress-reports/{id}", owner: "supervision", description: "Update a draft or returned progress report (re-snapshots the project percent).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/supervision/progress-reports/{id}/submit", owner: "supervision", description: "Submit a draft progress report for approval.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/supervision/progress-reports/{id}/approve", owner: "supervision", description: "Approve a submitted progress report (records an approval row).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/supervision/progress-reports/{id}/return", owner: "supervision", description: "Return a submitted progress report (comment required; records an approval row).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/supervision/defects", owner: "supervision", description: "List defects (paginated, filter by project_id, inspection_id, severity, status, task_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/supervision/defects", owner: "supervision", description: "Log a defect found on site (optionally linked to an inspection or a project task).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/supervision/defects/{id}", owner: "supervision", description: "Get one defect.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/supervision/defects/{id}", owner: "supervision", description: "Update a defect (forward-only status transitions).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/supervision/defects/{id}/follow-up", owner: "supervision", description: "Request a follow-up task in the projects module for a defect.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/supervision/defects/{id}/verify", owner: "supervision", description: "Verify a resolved defect (resolved -> verified).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/supervision/approvals", owner: "supervision", description: "List approval rows (paginated, filter by target_kind, target_id, approver_id, decision, project_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/supervision/daily-logs", owner: "supervision", description: "List site daily logs (paginated, filter by project_id, date, supervisor_id, signed).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/supervision/daily-logs", owner: "supervision", description: "Create a site daily log (one per project per day).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/supervision/daily-logs/{id}", owner: "supervision", description: "Get one daily log.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/supervision/daily-logs/{id}", owner: "supervision", description: "Update an unsigned daily log (signed-off logs are immutable).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/supervision/daily-logs/{id}/sign-off", owner: "supervision", description: "Supervisor sign-off for a daily log (only the log's supervisor).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/supervision/photos", owner: "supervision", description: "List photo evidence references (paginated, filter by project_id, subject_kind, subject_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/supervision/photos", owner: "supervision", description: "Attach photo evidence (storage reference + SHA-256 hash + caption; no bytes stored).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/supervision/photos/{id}", owner: "supervision", description: "Get one photo evidence record.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/supervision/photos/{id}", owner: "supervision", description: "Delete a photo evidence reference (the stored bytes are owned by the storage module and are not deleted).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SupervisionModule::new()));
}
