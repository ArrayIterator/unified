// Projects module — module definition and registration.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::web;
use serde_json::{Value, json};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct ProjectsModule {
    pub(crate) state: Arc<ProjectsState>,
}

impl ProjectsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(ProjectsState::new()),
        }
    }

    /// Every methodology as an admin-toggleable submodule.
    pub fn submodules(&self) -> &'static [MethodologySubmodule] {
        PROJECT_SUBMODULES
    }

    /// Product declaration for this module, for the product catalog.
    pub fn product_info(&self) -> Value {
        json!({
            "available_as_product": true,
            "product_category": "productivity",
            "module": "projects",
        })
    }

    /// Admin-readable declaration of this module: id, submodules,
    /// and product flag, without any trait change.
    pub fn metadata(&self) -> Value {
        json!({
            "id": "projects",
            "submodules": PROJECT_SUBMODULES,
            "available_as_product": AVAILABLE_AS_PRODUCT,
            "product_category": PRODUCT_CATEGORY,
        })
    }
}

impl Default for ProjectsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ProjectsModule {
    fn id(&self) -> &'static str {
        "projects"
    }

    fn name(&self) -> &'static str {
        "Project Management"
    }

    fn description(&self) -> &'static str {
        "Project management with a selectable methodology per project (Kanban, Scrum, Waterfall, Scrumban, Agile, Lean, XP, PRINCE2, PMBOK, CPM, CCPM, Six Sigma, Hybrid, Shape Up, and SAFe) over one shared data model: tasks, boards, sprints, phases, cycles, program increments, business cases, milestones, time tracking, automation rules, templates, and reports, wired into tickets, CRM, notifications, billing, and storage through hooks only. Each methodology is also declared as a submodule that the admin can enable per project, and the module is available as a product in the productivity category."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec![
            "tickets",
            "crm",
            "notifications",
            "billing",
            "storage",
            "roles",
        ]
    }

    /// Sellable-product declaration (batch 2): this module is
    /// available as a product in the productivity category, so the
    /// registry (`sellable_modules` / `product_offerings`) lists it
    /// and the licensing gate applies on enable. The same values
    /// are also exposed as the `AVAILABLE_AS_PRODUCT` /
    /// `PRODUCT_CATEGORY` constants and through `product_info()` /
    /// `metadata()`; this override is the one the registry reads.
    fn available_as_product(&self) -> bool {
        AVAILABLE_AS_PRODUCT
    }

    fn product_category(&self) -> Option<&'static str> {
        Some(PRODUCT_CATEGORY)
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("projects", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_task_from_ticket_requested(state.clone(), ctx)
        });
        bus.register("project.task_from_ticket_requested", "projects", handler);
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_ticket_from_task_requested(state.clone(), ctx)
        });
        bus.register("project.ticket_from_task_requested", "projects", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_billable_hours_requested(state.clone(), ctx));
        bus.register("project.billable_hours_requested", "projects", handler);
        // Supervision query hooks (batch 5 contract): project
        // lookup, live progress, follow-up task creation, and task
        // lookup. Supervision owns no project data and fails closed
        // until these answer.
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_project_get_requested(state.clone(), ctx));
        bus.register("project.get_requested", "projects", handler);
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_project_progress_requested(state.clone(), ctx)
        });
        bus.register("project.progress_requested", "projects", handler);
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_project_task_create_requested(state.clone(), ctx)
        });
        bus.register("project.task.create_requested", "projects", handler);
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_project_task_get_requested(state.clone(), ctx)
        });
        bus.register("project.task.get_requested", "projects", handler);
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/projects")
                // Literal segments first: they must win over `/{id}`.
                .route("/methodologies", web::get().to(list_methodologies))
                .route("/templates", web::get().to(list_project_templates))
                .route("/templates", web::post().to(create_project_template))
                .route("/templates/{id}", web::get().to(get_project_template))
                .route("/templates/{id}", web::put().to(update_project_template))
                .route("/templates/{id}", web::delete().to(delete_project_template))
                .route("", web::get().to(list_projects))
                .route("", web::post().to(create_project))
                .route("/{id}", web::get().to(get_project))
                .route("/{id}", web::put().to(update_project))
                .route("/{id}", web::delete().to(delete_project))
                .route("/{id}/archive", web::post().to(archive_project))
                .route("/{id}/restore", web::post().to(restore_project))
                .route("/{id}/overview", web::get().to(get_project_overview))
                .route("/{id}/members", web::get().to(list_project_members))
                .route("/{id}/members", web::post().to(add_project_member))
                .route(
                    "/{id}/members/{user_id}",
                    web::put().to(update_project_member),
                )
                .route(
                    "/{id}/members/{user_id}",
                    web::delete().to(remove_project_member),
                )
                .route(
                    "/{id}/apply-template",
                    web::post().to(apply_project_template),
                )
                .route("/{id}/boards", web::get().to(list_project_boards))
                .route("/{id}/boards", web::post().to(create_project_board))
                .route("/{id}/boards/{board_id}", web::get().to(get_project_board))
                .route(
                    "/{id}/boards/{board_id}",
                    web::put().to(update_project_board),
                )
                .route(
                    "/{id}/boards/{board_id}",
                    web::delete().to(delete_project_board),
                )
                .route(
                    "/{id}/boards/{board_id}/columns",
                    web::get().to(list_board_columns),
                )
                .route(
                    "/{id}/boards/{board_id}/columns",
                    web::post().to(create_board_column),
                )
                .route(
                    "/{id}/boards/{board_id}/columns/{column_id}",
                    web::put().to(update_board_column),
                )
                .route(
                    "/{id}/boards/{board_id}/columns/{column_id}",
                    web::delete().to(delete_board_column),
                )
                .route("/{id}/milestones", web::get().to(list_project_milestones))
                .route("/{id}/milestones", web::post().to(create_project_milestone))
                .route(
                    "/{id}/milestones/{milestone_id}",
                    web::get().to(get_project_milestone),
                )
                .route(
                    "/{id}/milestones/{milestone_id}",
                    web::put().to(update_project_milestone),
                )
                .route(
                    "/{id}/milestones/{milestone_id}",
                    web::delete().to(delete_project_milestone),
                )
                .route(
                    "/{id}/milestones/{milestone_id}/reach",
                    web::post().to(reach_project_milestone),
                )
                .route("/{id}/tasks", web::get().to(list_project_tasks))
                .route("/{id}/tasks", web::post().to(create_project_task))
                // Literal `from-ticket` must win over `/{task_id}`.
                .route(
                    "/{id}/tasks/from-ticket",
                    web::post().to(create_task_from_ticket),
                )
                .route("/{id}/tasks/{task_id}", web::get().to(get_project_task))
                .route("/{id}/tasks/{task_id}", web::put().to(update_project_task))
                .route(
                    "/{id}/tasks/{task_id}",
                    web::delete().to(delete_project_task),
                )
                .route(
                    "/{id}/tasks/{task_id}/move",
                    web::post().to(move_project_task),
                )
                .route(
                    "/{id}/tasks/{task_id}/assign",
                    web::post().to(assign_project_task),
                )
                .route(
                    "/{id}/tasks/{task_id}/complete",
                    web::post().to(complete_project_task),
                )
                .route(
                    "/{id}/tasks/{task_id}/reopen",
                    web::post().to(reopen_project_task),
                )
                .route(
                    "/{id}/tasks/{task_id}/subtasks",
                    web::get().to(list_task_subtasks),
                )
                .route(
                    "/{id}/tasks/{task_id}/dependencies",
                    web::get().to(list_task_dependencies),
                )
                .route(
                    "/{id}/tasks/{task_id}/dependencies",
                    web::post().to(add_task_dependency),
                )
                .route(
                    "/{id}/tasks/{task_id}/dependencies/{dependency_id}",
                    web::delete().to(remove_task_dependency),
                )
                .route(
                    "/{id}/tasks/{task_id}/comments",
                    web::get().to(list_task_comments),
                )
                .route(
                    "/{id}/tasks/{task_id}/comments",
                    web::post().to(add_task_comment),
                )
                .route(
                    "/{id}/tasks/{task_id}/comments/{comment_id}",
                    web::put().to(update_task_comment),
                )
                .route(
                    "/{id}/tasks/{task_id}/comments/{comment_id}",
                    web::delete().to(delete_task_comment),
                )
                .route(
                    "/{id}/tasks/{task_id}/watchers",
                    web::get().to(list_task_watchers),
                )
                .route(
                    "/{id}/tasks/{task_id}/watchers",
                    web::post().to(add_task_watcher),
                )
                .route(
                    "/{id}/tasks/{task_id}/watchers/{user_id}",
                    web::delete().to(remove_task_watcher),
                )
                .route(
                    "/{id}/tasks/{task_id}/attachments",
                    web::get().to(list_task_attachments),
                )
                .route(
                    "/{id}/tasks/{task_id}/attachments",
                    web::post().to(add_task_attachment),
                )
                .route(
                    "/{id}/tasks/{task_id}/attachments/{attachment_id}",
                    web::delete().to(delete_task_attachment),
                )
                .route(
                    "/{id}/tasks/{task_id}/convert-to-ticket",
                    web::post().to(convert_task_to_ticket),
                )
                .route(
                    "/{id}/automation-rules",
                    web::get().to(list_automation_rules),
                )
                .route(
                    "/{id}/automation-rules",
                    web::post().to(create_automation_rule),
                )
                .route(
                    "/{id}/automation-rules/{rule_id}",
                    web::get().to(get_automation_rule),
                )
                .route(
                    "/{id}/automation-rules/{rule_id}",
                    web::put().to(update_automation_rule),
                )
                .route(
                    "/{id}/automation-rules/{rule_id}",
                    web::delete().to(delete_automation_rule),
                )
                .route(
                    "/{id}/automation-rules/{rule_id}/run",
                    web::post().to(run_automation_rule),
                )
                .route("/{id}/sprints", web::get().to(list_project_sprints))
                .route("/{id}/sprints", web::post().to(create_project_sprint))
                .route(
                    "/{id}/sprints/{sprint_id}",
                    web::get().to(get_project_sprint),
                )
                .route(
                    "/{id}/sprints/{sprint_id}",
                    web::put().to(update_project_sprint),
                )
                .route(
                    "/{id}/sprints/{sprint_id}",
                    web::delete().to(delete_project_sprint),
                )
                .route(
                    "/{id}/sprints/{sprint_id}/start",
                    web::post().to(start_project_sprint),
                )
                .route(
                    "/{id}/sprints/{sprint_id}/close",
                    web::post().to(close_project_sprint),
                )
                .route(
                    "/{id}/sprints/{sprint_id}/standup",
                    web::post().to(add_sprint_standup),
                )
                .route(
                    "/{id}/sprints/{sprint_id}/review",
                    web::post().to(save_sprint_review),
                )
                .route(
                    "/{id}/sprints/{sprint_id}/retrospective",
                    web::post().to(save_sprint_retrospective),
                )
                .route("/{id}/phases", web::get().to(list_project_phases))
                .route("/{id}/phases", web::post().to(create_project_phase))
                .route("/{id}/phases/{phase_id}", web::get().to(get_project_phase))
                .route(
                    "/{id}/phases/{phase_id}",
                    web::put().to(update_project_phase),
                )
                .route(
                    "/{id}/phases/{phase_id}",
                    web::delete().to(delete_project_phase),
                )
                .route(
                    "/{id}/phases/{phase_id}/gate",
                    web::post().to(decide_phase_gate),
                )
                .route(
                    "/{id}/phases/{phase_id}/complete",
                    web::post().to(complete_project_phase),
                )
                .route("/{id}/cycles", web::get().to(list_project_cycles))
                .route("/{id}/cycles", web::post().to(create_project_cycle))
                .route("/{id}/cycles/{cycle_id}", web::get().to(get_project_cycle))
                .route(
                    "/{id}/cycles/{cycle_id}",
                    web::put().to(update_project_cycle),
                )
                .route(
                    "/{id}/cycles/{cycle_id}",
                    web::delete().to(delete_project_cycle),
                )
                .route(
                    "/{id}/cycles/{cycle_id}/bet",
                    web::post().to(bet_project_cycle),
                )
                .route(
                    "/{id}/cycles/{cycle_id}/complete",
                    web::post().to(complete_project_cycle),
                )
                .route(
                    "/{id}/program-increments",
                    web::get().to(list_program_increments),
                )
                .route(
                    "/{id}/program-increments",
                    web::post().to(create_program_increment),
                )
                .route(
                    "/{id}/program-increments/{pi_id}",
                    web::get().to(get_program_increment),
                )
                .route(
                    "/{id}/program-increments/{pi_id}",
                    web::put().to(update_program_increment),
                )
                .route(
                    "/{id}/program-increments/{pi_id}",
                    web::delete().to(delete_program_increment),
                )
                .route(
                    "/{id}/program-increments/{pi_id}/plan",
                    web::post().to(plan_program_increment),
                )
                .route("/{id}/business-cases", web::get().to(list_business_cases))
                .route("/{id}/business-cases", web::post().to(create_business_case))
                .route(
                    "/{id}/business-cases/{case_id}",
                    web::get().to(get_business_case),
                )
                .route(
                    "/{id}/business-cases/{case_id}",
                    web::put().to(update_business_case),
                )
                .route(
                    "/{id}/business-cases/{case_id}",
                    web::delete().to(delete_business_case),
                )
                .route(
                    "/{id}/business-cases/{case_id}/exception-report",
                    web::post().to(save_exception_report),
                )
                .route("/{id}/time-entries", web::get().to(list_time_entries))
                .route("/{id}/time-entries", web::post().to(create_time_entry))
                // Literal `billable` must win over `/{entry_id}`.
                .route(
                    "/{id}/time-entries/billable",
                    web::get().to(list_billable_time),
                )
                .route(
                    "/{id}/time-entries/{entry_id}",
                    web::get().to(get_time_entry),
                )
                .route(
                    "/{id}/time-entries/{entry_id}",
                    web::put().to(update_time_entry),
                )
                .route(
                    "/{id}/time-entries/{entry_id}",
                    web::delete().to(delete_time_entry),
                )
                .route(
                    "/{id}/time-entries/{entry_id}/start-timer",
                    web::post().to(start_time_timer),
                )
                .route(
                    "/{id}/time-entries/{entry_id}/stop-timer",
                    web::post().to(stop_time_timer),
                )
                .route("/{id}/workload", web::get().to(get_project_workload))
                .route("/{id}/gantt", web::get().to(get_project_gantt))
                .route("/{id}/timeline", web::get().to(get_project_timeline))
                .route("/{id}/calendar", web::get().to(get_project_calendar))
                .route("/{id}/reports/burndown", web::get().to(get_burndown_report))
                .route("/{id}/reports/velocity", web::get().to(get_velocity_report))
                .route(
                    "/{id}/reports/cumulative-flow",
                    web::get().to(get_cumulative_flow_report),
                )
                .route(
                    "/{id}/reports/cycle-time",
                    web::get().to(get_cycle_time_report),
                )
                .route(
                    "/{id}/reports/critical-path",
                    web::get().to(get_critical_path_report),
                )
                .route("/{id}/export", web::get().to(export_project))
                .route("/{id}/import", web::post().to(import_project))
                .route("/{id}/activity", web::get().to(list_project_activity)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects",
                owner: "projects",
                description: "List projects (paginated; viewer-scoped).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects",
                owner: "projects",
                description: "Create a project with a methodology.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/methodologies",
                owner: "projects",
                description: "List the 15 supported methodologies with their defaults.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/templates",
                owner: "projects",
                description: "List project templates (paginated).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/templates",
                owner: "projects",
                description: "Create a project template.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/templates/{id}",
                owner: "projects",
                description: "Get one project template.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/projects/templates/{id}",
                owner: "projects",
                description: "Update a project template.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/projects/templates/{id}",
                owner: "projects",
                description: "Delete a project template.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}",
                owner: "projects",
                description: "Get one project.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/projects/{id}",
                owner: "projects",
                description: "Update a project.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/projects/{id}",
                owner: "projects",
                description: "Delete a project.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/archive",
                owner: "projects",
                description: "Archive a project.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/restore",
                owner: "projects",
                description: "Restore an archived project.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/overview",
                owner: "projects",
                description: "Get a project overview (counts and progress).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/members",
                owner: "projects",
                description: "List project members.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/members",
                owner: "projects",
                description: "Add a member to a project.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/projects/{id}/members/{user_id}",
                owner: "projects",
                description: "Update a project member's role.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/projects/{id}/members/{user_id}",
                owner: "projects",
                description: "Remove a member from a project.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/apply-template",
                owner: "projects",
                description: "Apply a template (boards, columns, phases, custom fields) to a project.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/boards",
                owner: "projects",
                description: "List project boards.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/boards",
                owner: "projects",
                description: "Create a project board.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/boards/{board_id}",
                owner: "projects",
                description: "Get one project board.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/projects/{id}/boards/{board_id}",
                owner: "projects",
                description: "Update a project board.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/projects/{id}/boards/{board_id}",
                owner: "projects",
                description: "Delete a project board.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/boards/{board_id}/columns",
                owner: "projects",
                description: "List board columns.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/boards/{board_id}/columns",
                owner: "projects",
                description: "Create a board column.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/projects/{id}/boards/{board_id}/columns/{column_id}",
                owner: "projects",
                description: "Update a board column.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/projects/{id}/boards/{board_id}/columns/{column_id}",
                owner: "projects",
                description: "Delete a board column.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/milestones",
                owner: "projects",
                description: "List project milestones.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/milestones",
                owner: "projects",
                description: "Create a project milestone.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/milestones/{milestone_id}",
                owner: "projects",
                description: "Get one project milestone.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/projects/{id}/milestones/{milestone_id}",
                owner: "projects",
                description: "Update a project milestone.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/projects/{id}/milestones/{milestone_id}",
                owner: "projects",
                description: "Delete a project milestone.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/milestones/{milestone_id}/reach",
                owner: "projects",
                description: "Mark a milestone as reached.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/tasks",
                owner: "projects",
                description: "List project tasks (paginated; filter by status, assignee, sprint, phase, labels).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/tasks",
                owner: "projects",
                description: "Create a project task.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/tasks/{task_id}",
                owner: "projects",
                description: "Get one project task.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/projects/{id}/tasks/{task_id}",
                owner: "projects",
                description: "Update a project task.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/projects/{id}/tasks/{task_id}",
                owner: "projects",
                description: "Delete a project task.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/tasks/{task_id}/move",
                owner: "projects",
                description: "Move a task to another board, column, or status.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/tasks/{task_id}/assign",
                owner: "projects",
                description: "Assign a task to a user.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/tasks/{task_id}/complete",
                owner: "projects",
                description: "Mark a task as completed.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/tasks/{task_id}/reopen",
                owner: "projects",
                description: "Reopen a completed task.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/tasks/{task_id}/subtasks",
                owner: "projects",
                description: "List a task's subtasks.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/tasks/{task_id}/dependencies",
                owner: "projects",
                description: "List a task's dependencies.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/tasks/{task_id}/dependencies",
                owner: "projects",
                description: "Add a dependency to a task.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/projects/{id}/tasks/{task_id}/dependencies/{dependency_id}",
                owner: "projects",
                description: "Remove a dependency from a task.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/tasks/{task_id}/comments",
                owner: "projects",
                description: "List a task's comments.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/tasks/{task_id}/comments",
                owner: "projects",
                description: "Add a comment to a task.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/projects/{id}/tasks/{task_id}/comments/{comment_id}",
                owner: "projects",
                description: "Update a task comment.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/projects/{id}/tasks/{task_id}/comments/{comment_id}",
                owner: "projects",
                description: "Delete a task comment.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/tasks/{task_id}/watchers",
                owner: "projects",
                description: "List a task's watchers.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/tasks/{task_id}/watchers",
                owner: "projects",
                description: "Watch a task.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/projects/{id}/tasks/{task_id}/watchers/{user_id}",
                owner: "projects",
                description: "Stop watching a task.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/tasks/{task_id}/attachments",
                owner: "projects",
                description: "List a task's attachments (storage references).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/tasks/{task_id}/attachments",
                owner: "projects",
                description: "Attach a file already uploaded through the Storage module.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/projects/{id}/tasks/{task_id}/attachments/{attachment_id}",
                owner: "projects",
                description: "Delete a task attachment reference.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/automation-rules",
                owner: "projects",
                description: "List project automation rules.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/automation-rules",
                owner: "projects",
                description: "Create a project automation rule.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/automation-rules/{rule_id}",
                owner: "projects",
                description: "Get one automation rule.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/projects/{id}/automation-rules/{rule_id}",
                owner: "projects",
                description: "Update an automation rule.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/projects/{id}/automation-rules/{rule_id}",
                owner: "projects",
                description: "Delete an automation rule.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/automation-rules/{rule_id}/run",
                owner: "projects",
                description: "Run an automation rule now.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/tasks/{task_id}/convert-to-ticket",
                owner: "projects",
                description: "Convert a task into a support ticket (hook).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/tasks/from-ticket",
                owner: "projects",
                description: "Create a task from a support ticket (hook).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/sprints",
                owner: "projects",
                description: "List project sprints.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/sprints",
                owner: "projects",
                description: "Create a project sprint.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/sprints/{sprint_id}",
                owner: "projects",
                description: "Get one project sprint.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/projects/{id}/sprints/{sprint_id}",
                owner: "projects",
                description: "Update a project sprint.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/projects/{id}/sprints/{sprint_id}",
                owner: "projects",
                description: "Delete a project sprint.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/sprints/{sprint_id}/start",
                owner: "projects",
                description: "Start a sprint.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/sprints/{sprint_id}/close",
                owner: "projects",
                description: "Close a sprint and record its velocity.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/sprints/{sprint_id}/standup",
                owner: "projects",
                description: "Add a standup note to a sprint.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/sprints/{sprint_id}/review",
                owner: "projects",
                description: "Save a sprint review.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/sprints/{sprint_id}/retrospective",
                owner: "projects",
                description: "Save a sprint retrospective.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/phases",
                owner: "projects",
                description: "List project phases.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/phases",
                owner: "projects",
                description: "Create a project phase.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/phases/{phase_id}",
                owner: "projects",
                description: "Get one project phase.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/projects/{id}/phases/{phase_id}",
                owner: "projects",
                description: "Update a project phase.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/projects/{id}/phases/{phase_id}",
                owner: "projects",
                description: "Delete a project phase.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/phases/{phase_id}/gate",
                owner: "projects",
                description: "Decide a phase gate (approve or reject).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/phases/{phase_id}/complete",
                owner: "projects",
                description: "Complete a project phase.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/cycles",
                owner: "projects",
                description: "List Shape Up cycles.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/cycles",
                owner: "projects",
                description: "Create a Shape Up cycle.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/cycles/{cycle_id}",
                owner: "projects",
                description: "Get one Shape Up cycle.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/projects/{id}/cycles/{cycle_id}",
                owner: "projects",
                description: "Update a Shape Up cycle.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/projects/{id}/cycles/{cycle_id}",
                owner: "projects",
                description: "Delete a Shape Up cycle.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/cycles/{cycle_id}/bet",
                owner: "projects",
                description: "Record a betting decision for a cycle.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/cycles/{cycle_id}/complete",
                owner: "projects",
                description: "Complete a Shape Up cycle.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/program-increments",
                owner: "projects",
                description: "List SAFe program increments.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/program-increments",
                owner: "projects",
                description: "Create a program increment.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/program-increments/{pi_id}",
                owner: "projects",
                description: "Get one program increment.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/projects/{id}/program-increments/{pi_id}",
                owner: "projects",
                description: "Update a program increment.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/projects/{id}/program-increments/{pi_id}",
                owner: "projects",
                description: "Delete a program increment.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/program-increments/{pi_id}/plan",
                owner: "projects",
                description: "Plan a program increment (objectives).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/business-cases",
                owner: "projects",
                description: "List PRINCE2 business cases.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/business-cases",
                owner: "projects",
                description: "Create a business case.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/business-cases/{case_id}",
                owner: "projects",
                description: "Get one business case.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/projects/{id}/business-cases/{case_id}",
                owner: "projects",
                description: "Update a business case.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/projects/{id}/business-cases/{case_id}",
                owner: "projects",
                description: "Delete a business case.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/business-cases/{case_id}/exception-report",
                owner: "projects",
                description: "Save an exception report for a business case.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/time-entries",
                owner: "projects",
                description: "List project time entries.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/time-entries",
                owner: "projects",
                description: "Create a time entry.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/time-entries/billable",
                owner: "projects",
                description: "List unbilled billable time entries.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/time-entries/{entry_id}",
                owner: "projects",
                description: "Get one time entry.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/projects/{id}/time-entries/{entry_id}",
                owner: "projects",
                description: "Update a time entry.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/projects/{id}/time-entries/{entry_id}",
                owner: "projects",
                description: "Delete a time entry.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/time-entries/{entry_id}/start-timer",
                owner: "projects",
                description: "Start a running timer on a time entry.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/time-entries/{entry_id}/stop-timer",
                owner: "projects",
                description: "Stop a running timer and record its hours.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/workload",
                owner: "projects",
                description: "Get per-member workload for a project.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/gantt",
                owner: "projects",
                description: "Get a project's Gantt view (tasks and dependencies).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/timeline",
                owner: "projects",
                description: "Get a project's timeline view.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/calendar",
                owner: "projects",
                description: "Get a project's calendar view.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/reports/burndown",
                owner: "projects",
                description: "Get the sprint burndown report.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/reports/velocity",
                owner: "projects",
                description: "Get the velocity report.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/reports/cumulative-flow",
                owner: "projects",
                description: "Get the cumulative flow report.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/reports/cycle-time",
                owner: "projects",
                description: "Get the cycle time report.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/reports/critical-path",
                owner: "projects",
                description: "Get the critical path report.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/export",
                owner: "projects",
                description: "Export a project (tasks, boards, sprints, phases, time entries).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/projects/{id}/import",
                owner: "projects",
                description: "Import a project export.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/projects/{id}/activity",
                owner: "projects",
                description: "List a project's activity feed.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ProjectsModule::new()));
}
