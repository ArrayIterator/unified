// Project Management module.
//
// Every project picks one methodology at creation time, and that choice
// drives its defaults: board kind, columns, statuses, and whether it
// uses sprints, phases, WIP limits, story points, Shape Up cycles, or
// SAFe program increments. Fifteen methodologies share one data model
// (projects, boards, tasks, sprints, phases, cycles, program
// increments, business cases, time entries, automation rules,
// templates), so a project can change methodology without losing its
// work, and reports can aggregate across methodologies.
//
// Interconnection happens only through the hook bus: tasks can be
// created from support tickets and converted back into tickets, and
// billable hours are exposed to the billing module through a hook.
// Nothing here calls another module directly.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext, HookHandler};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Entities
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub description: String,
    pub methodology: String,
    pub status: String,
    pub visibility: String,
    pub owner_id: String,
    pub company_id: Option<String>,
    pub client_id: Option<String>,
    pub start_date: Option<DateTime<Utc>>,
    pub due_date: Option<DateTime<Utc>>,
    pub color: Option<String>,
    pub icon: Option<String>,
    pub labels: Vec<String>,
    pub custom_field_definitions: Value,
    pub archived: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMember {
    pub id: String,
    pub project_id: String,
    pub user_id: String,
    /// `owner`, `manager`, `member`, or `viewer`.
    pub role: String,
    pub joined_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectBoard {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub kind: String,
    pub wip_limit: Option<u32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectBoardColumn {
    pub id: String,
    pub board_id: String,
    pub name: String,
    pub position: u32,
    pub status_key: String,
    pub wip_limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectTask {
    pub id: String,
    pub project_id: String,
    pub board_id: Option<String>,
    pub parent_id: Option<String>,
    pub epic_id: Option<String>,
    pub title: String,
    pub description: String,
    /// `task`, `story`, `bug`, `spike`, `feature`, `enabler`, or `chore`.
    pub task_type: String,
    pub status: String,
    pub priority: String,
    pub assignee_id: Option<String>,
    pub reporter_id: String,
    pub estimate_points: Option<f64>,
    pub estimate_hours: Option<f64>,
    pub labels: Vec<String>,
    pub custom_values: Value,
    pub start_date: Option<DateTime<Utc>>,
    pub due_date: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub sprint_id: Option<String>,
    pub phase_id: Option<String>,
    pub cycle_id: Option<String>,
    pub pi_id: Option<String>,
    pub milestone_id: Option<String>,
    pub process_group: Option<String>,
    pub knowledge_area: Option<String>,
    pub waste_flag: Option<bool>,
    pub pair_partner_id: Option<String>,
    pub test_first: bool,
    pub dmaic_phase: Option<String>,
    pub appetite_weeks: Option<u32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectTaskDependency {
    pub id: String,
    pub task_id: String,
    pub depends_on_task_id: String,
    /// `finish_start`, `start_start`, `finish_finish`, `start_finish`,
    /// `blocks`, or `related`.
    pub kind: String,
    pub lag_days: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectTaskComment {
    pub id: String,
    pub task_id: String,
    pub author_id: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectTaskWatcher {
    pub id: String,
    pub task_id: String,
    pub user_id: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectTaskAttachment {
    pub id: String,
    pub task_id: String,
    pub target: String,
    pub key: String,
    pub stored_key: String,
    pub filename: String,
    pub content_type: Option<String>,
    pub size_bytes: Option<u64>,
    pub uploaded_by: String,
    pub uploaded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMilestone {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub description: String,
    pub due_date: Option<DateTime<Utc>>,
    pub reached: bool,
    pub reached_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSprint {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub goal: String,
    /// `planned`, `active`, or `completed`.
    pub status: String,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
    pub velocity_planned: Option<f64>,
    pub velocity_actual: Option<f64>,
    pub standup_notes: Value,
    pub review_notes: Option<String>,
    pub retrospective_notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectPhase {
    pub id: String,
    pub project_id: String,
    pub name: String,
    /// `waterfall`, `prince2_stage`, `dmaic`, or `hybrid`.
    pub kind: String,
    pub position: u32,
    /// `planned`, `active`, or `completed`.
    pub status: String,
    /// `pending`, `approved`, or `rejected`.
    pub gate_status: String,
    pub gate_decision: Option<String>,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectTimeEntry {
    pub id: String,
    pub task_id: String,
    pub project_id: String,
    pub user_id: String,
    pub description: String,
    pub hours: f64,
    pub billable: bool,
    pub billed: bool,
    pub invoice_id: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub timer_running: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectAutomationRule {
    pub id: String,
    pub project_id: String,
    pub name: String,
    /// `task_created`, `status_changed`, `task_completed`, `due_soon`,
    /// or `label_added`.
    pub trigger: String,
    pub conditions: Value,
    pub actions: Value,
    pub enabled: bool,
    pub run_count: u64,
    pub last_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
    pub methodology: String,
    pub boards: Value,
    pub columns: Value,
    pub statuses: Value,
    pub phases: Value,
    pub custom_fields: Value,
    pub is_system: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectCycle {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub appetite_weeks: u32,
    /// `shaping`, `betting`, `building`, `cooldown`, or `done`.
    pub status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub cooldown_weeks: u32,
    pub pitch: Option<String>,
    pub betting_decision: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectProgramIncrement {
    pub id: String,
    pub project_id: String,
    pub name: String,
    /// `planned`, `active`, or `completed`.
    pub status: String,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
    pub objectives: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectBusinessCase {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub justification: String,
    pub benefits: Value,
    pub costs: Value,
    pub risks: Value,
    /// `draft`, `approved`, or `rejected`.
    pub status: String,
    pub exception_report: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectActivity {
    pub id: String,
    pub project_id: String,
    pub actor_id: String,
    pub action: String,
    pub target_type: String,
    pub target_id: Option<String>,
    pub details: Value,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Methodologies
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct MethodologyProfile {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub default_statuses: &'static [&'static str],
    pub default_columns: &'static [&'static str],
    pub uses_sprints: bool,
    pub uses_phases: bool,
    pub uses_wip_limits: bool,
    pub uses_story_points: bool,
    pub uses_cycles: bool,
    pub uses_program_increments: bool,
    pub default_board_kind: &'static str,
}

pub const METHODOLOGIES: &[MethodologyProfile] = &[
    MethodologyProfile {
        id: "kanban",
        name: "Kanban",
        description: "Continuous flow board with WIP limits; work is pulled through stages instead of planned in timeboxes.",
        default_statuses: &["backlog", "ready", "in_progress", "review", "done"],
        default_columns: &["backlog", "ready", "in_progress", "review", "done"],
        uses_sprints: false,
        uses_phases: false,
        uses_wip_limits: true,
        uses_story_points: false,
        uses_cycles: false,
        uses_program_increments: false,
        default_board_kind: "kanban",
    },
    MethodologyProfile {
        id: "scrum",
        name: "Scrum",
        description: "Timeboxed sprints with a product backlog, sprint planning, daily standups, review, and retrospective.",
        default_statuses: &["backlog", "sprint_backlog", "in_progress", "review", "done"],
        default_columns: &["backlog", "sprint_backlog", "in_progress", "review", "done"],
        uses_sprints: true,
        uses_phases: false,
        uses_wip_limits: false,
        uses_story_points: true,
        uses_cycles: false,
        uses_program_increments: false,
        default_board_kind: "scrum",
    },
    MethodologyProfile {
        id: "waterfall",
        name: "Waterfall",
        description: "Sequential phases with gate reviews; each phase finishes before the next one starts.",
        default_statuses: &["not_started", "in_progress", "blocked", "done"],
        default_columns: &["not_started", "in_progress", "blocked", "done"],
        uses_sprints: false,
        uses_phases: true,
        uses_wip_limits: false,
        uses_story_points: false,
        uses_cycles: false,
        uses_program_increments: false,
        default_board_kind: "waterfall",
    },
    MethodologyProfile {
        id: "scrumban",
        name: "Scrumban",
        description: "Scrum sprints on a Kanban flow board: timeboxed planning with WIP limits and pull-based execution.",
        default_statuses: &["backlog", "ready", "in_progress", "review", "done"],
        default_columns: &["backlog", "ready", "in_progress", "review", "done"],
        uses_sprints: true,
        uses_phases: false,
        uses_wip_limits: true,
        uses_story_points: true,
        uses_cycles: false,
        uses_program_increments: false,
        default_board_kind: "scrumban",
    },
    MethodologyProfile {
        id: "agile",
        name: "Agile",
        description: "Iterative delivery in short iterations without mandatory Scrum ceremonies; working software each iteration.",
        default_statuses: &["backlog", "in_progress", "review", "done"],
        default_columns: &["backlog", "in_progress", "review", "done"],
        uses_sprints: true,
        uses_phases: false,
        uses_wip_limits: false,
        uses_story_points: true,
        uses_cycles: false,
        uses_program_increments: false,
        default_board_kind: "agile",
    },
    MethodologyProfile {
        id: "lean",
        name: "Lean",
        description: "Value-stream flow with pull scheduling; waste is flagged on tasks and removed from the stream.",
        default_statuses: &["backlog", "in_progress", "review", "done"],
        default_columns: &["backlog", "in_progress", "review", "done"],
        uses_sprints: false,
        uses_phases: false,
        uses_wip_limits: true,
        uses_story_points: false,
        uses_cycles: false,
        uses_program_increments: false,
        default_board_kind: "kanban",
    },
    MethodologyProfile {
        id: "xp",
        name: "Extreme Programming",
        description: "Engineering-driven iterations with spikes, test-first development, and pair programming.",
        default_statuses: &["backlog", "spike", "in_progress", "testing", "done"],
        default_columns: &["backlog", "spike", "in_progress", "testing", "done"],
        uses_sprints: true,
        uses_phases: false,
        uses_wip_limits: false,
        uses_story_points: true,
        uses_cycles: false,
        uses_program_increments: false,
        default_board_kind: "scrum",
    },
    MethodologyProfile {
        id: "prince2",
        name: "PRINCE2",
        description: "Stage-gated delivery with a business case, stage gates, and exception reporting to the project board.",
        default_statuses: &["not_started", "in_progress", "blocked", "done"],
        default_columns: &["not_started", "in_progress", "blocked", "done"],
        uses_sprints: false,
        uses_phases: true,
        uses_wip_limits: false,
        uses_story_points: false,
        uses_cycles: false,
        uses_program_increments: false,
        default_board_kind: "waterfall",
    },
    MethodologyProfile {
        id: "pmbok",
        name: "PMBOK",
        description: "Process-group delivery (initiating, planning, executing, monitoring, closing) with knowledge-area tags on tasks.",
        default_statuses: &["not_started", "in_progress", "blocked", "done"],
        default_columns: &["not_started", "in_progress", "blocked", "done"],
        uses_sprints: false,
        uses_phases: true,
        uses_wip_limits: false,
        uses_story_points: false,
        uses_cycles: false,
        uses_program_increments: false,
        default_board_kind: "waterfall",
    },
    MethodologyProfile {
        id: "cpm",
        name: "Critical Path Method",
        description: "Dependency-driven scheduling on a task DAG; the critical path is the longest chain of dependent tasks.",
        default_statuses: &["not_started", "in_progress", "blocked", "done"],
        default_columns: &["not_started", "in_progress", "blocked", "done"],
        uses_sprints: false,
        uses_phases: false,
        uses_wip_limits: false,
        uses_story_points: false,
        uses_cycles: false,
        uses_program_increments: false,
        default_board_kind: "gantt",
    },
    MethodologyProfile {
        id: "ccpm",
        name: "Critical Chain Project Management",
        description: "Resource-constrained scheduling with buffers; tasks are sequenced around shared resources, not just dependencies.",
        default_statuses: &["not_started", "in_progress", "blocked", "done"],
        default_columns: &["not_started", "in_progress", "blocked", "done"],
        uses_sprints: false,
        uses_phases: false,
        uses_wip_limits: false,
        uses_story_points: false,
        uses_cycles: false,
        uses_program_increments: false,
        default_board_kind: "gantt",
    },
    MethodologyProfile {
        id: "six_sigma",
        name: "Six Sigma",
        description: "DMAIC improvement cycles (define, measure, analyze, improve, control) with gated phases.",
        default_statuses: &["not_started", "in_progress", "review", "done"],
        default_columns: &["not_started", "in_progress", "review", "done"],
        uses_sprints: false,
        uses_phases: true,
        uses_wip_limits: false,
        uses_story_points: false,
        uses_cycles: false,
        uses_program_increments: false,
        default_board_kind: "waterfall",
    },
    MethodologyProfile {
        id: "hybrid",
        name: "Hybrid",
        description: "Waterfall phases for planning and governance with agile execution inside each phase.",
        default_statuses: &["backlog", "in_progress", "review", "done"],
        default_columns: &["backlog", "in_progress", "review", "done"],
        uses_sprints: true,
        uses_phases: true,
        uses_wip_limits: false,
        uses_story_points: true,
        uses_cycles: false,
        uses_program_increments: false,
        default_board_kind: "hybrid",
    },
    MethodologyProfile {
        id: "shape_up",
        name: "Shape Up",
        description: "Six-week cycles with appetite limits, shaping, betting, and a cool-down between cycles.",
        default_statuses: &["backlog", "shaping", "in_progress", "review", "done"],
        default_columns: &["backlog", "shaping", "in_progress", "review", "done"],
        uses_sprints: false,
        uses_phases: false,
        uses_wip_limits: false,
        uses_story_points: false,
        uses_cycles: true,
        uses_program_increments: false,
        default_board_kind: "kanban",
    },
    MethodologyProfile {
        id: "safe",
        name: "SAFe",
        description: "Scaled Agile with program increments, an agile release train, and features plus enablers across teams.",
        default_statuses: &["backlog", "in_progress", "review", "done"],
        default_columns: &["backlog", "in_progress", "review", "done"],
        uses_sprints: true,
        uses_phases: false,
        uses_wip_limits: false,
        uses_story_points: true,
        uses_cycles: false,
        uses_program_increments: true,
        default_board_kind: "scrum",
    },
];

pub fn methodology_profile(id: &str) -> Option<&'static MethodologyProfile> {
    METHODOLOGIES.iter().find(|profile| profile.id == id)
}

pub fn valid_methodology(id: &str) -> bool {
    methodology_profile(id).is_some()
}

// ---------------------------------------------------------------------------
// Submodule declarations and product flag
// ---------------------------------------------------------------------------
//
// The core registry currently has no submodule mechanism and no
// available-as-product flag, so both are declared here in plain data
// the integrator can wire to the registry rule later, without any
// trait change: `PROJECT_SUBMODULES` lists every methodology as an
// admin-toggleable submodule, and `AVAILABLE_AS_PRODUCT` /
// `PRODUCT_CATEGORY` declare this module as a sellable product.
// `ProjectsModule::metadata` exposes both declarations as JSON.

/// One methodology exposed to the admin UI as a submodule that can
/// be enabled per project.
#[derive(Debug, Clone, Serialize)]
pub struct MethodologySubmodule {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub enabled_by_default: bool,
}

pub const PROJECT_SUBMODULES: &[MethodologySubmodule] = &[
    MethodologySubmodule {
        id: "kanban",
        name: "Kanban",
        description: "Enables the continuous flow board with WIP limits.",
        enabled_by_default: true,
    },
    MethodologySubmodule {
        id: "scrum",
        name: "Scrum",
        description: "Enables timeboxed sprints with standups, review, and retrospective.",
        enabled_by_default: true,
    },
    MethodologySubmodule {
        id: "waterfall",
        name: "Waterfall",
        description: "Enables sequential phases with gate reviews.",
        enabled_by_default: true,
    },
    MethodologySubmodule {
        id: "scrumban",
        name: "Scrumban",
        description: "Enables sprints on a WIP-limited flow board.",
        enabled_by_default: false,
    },
    MethodologySubmodule {
        id: "agile",
        name: "Agile",
        description: "Enables short iterations without mandatory Scrum ceremonies.",
        enabled_by_default: false,
    },
    MethodologySubmodule {
        id: "lean",
        name: "Lean",
        description: "Enables value-stream flow with waste flags on tasks.",
        enabled_by_default: false,
    },
    MethodologySubmodule {
        id: "xp",
        name: "XP",
        description: "Enables spikes, test-first development, and pair programming.",
        enabled_by_default: false,
    },
    MethodologySubmodule {
        id: "prince2",
        name: "PRINCE2",
        description: "Enables stage gates, business cases, and exception reporting.",
        enabled_by_default: false,
    },
    MethodologySubmodule {
        id: "pmbok",
        name: "PMBOK",
        description: "Enables process-group phases with knowledge-area tags.",
        enabled_by_default: false,
    },
    MethodologySubmodule {
        id: "cpm",
        name: "Critical Path Method",
        description: "Enables dependency-driven scheduling and critical path reports.",
        enabled_by_default: false,
    },
    MethodologySubmodule {
        id: "ccpm",
        name: "Critical Chain Project Management",
        description: "Enables resource-constrained scheduling with buffers.",
        enabled_by_default: false,
    },
    MethodologySubmodule {
        id: "six_sigma",
        name: "Six Sigma",
        description: "Enables DMAIC improvement cycles with gated phases.",
        enabled_by_default: false,
    },
    MethodologySubmodule {
        id: "hybrid",
        name: "Hybrid",
        description: "Enables waterfall phases with agile execution inside each phase.",
        enabled_by_default: false,
    },
    MethodologySubmodule {
        id: "shape_up",
        name: "Shape Up",
        description: "Enables six-week cycles with appetite, betting, and cool-down.",
        enabled_by_default: false,
    },
    MethodologySubmodule {
        id: "safe",
        name: "SAFe",
        description: "Enables program increments for scaled delivery across teams.",
        enabled_by_default: false,
    },
];

pub fn methodology_submodule(id: &str) -> Option<&'static MethodologySubmodule> {
    PROJECT_SUBMODULES.iter().find(|submodule| submodule.id == id)
}

/// True when this module can be sold as a product.
pub const AVAILABLE_AS_PRODUCT: bool = true;

/// Product category this module belongs to when sold as a product.
pub const PRODUCT_CATEGORY: &str = "productivity";

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct ProjectsState {
    hooks: RwLock<Arc<HookBus>>,
    projects: DashMap<String, Project>,
    members: DashMap<String, ProjectMember>,
    boards: DashMap<String, ProjectBoard>,
    columns: DashMap<String, ProjectBoardColumn>,
    tasks: DashMap<String, ProjectTask>,
    dependencies: DashMap<String, ProjectTaskDependency>,
    comments: DashMap<String, ProjectTaskComment>,
    watchers: DashMap<String, ProjectTaskWatcher>,
    attachments: DashMap<String, ProjectTaskAttachment>,
    milestones: DashMap<String, ProjectMilestone>,
    sprints: DashMap<String, ProjectSprint>,
    phases: DashMap<String, ProjectPhase>,
    time_entries: DashMap<String, ProjectTimeEntry>,
    automation_rules: DashMap<String, ProjectAutomationRule>,
    templates: DashMap<String, ProjectTemplate>,
    cycles: DashMap<String, ProjectCycle>,
    program_increments: DashMap<String, ProjectProgramIncrement>,
    business_cases: DashMap<String, ProjectBusinessCase>,
    activity: DashMap<String, ProjectActivity>,
}

impl ProjectsState {
    pub fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            projects: DashMap::new(),
            members: DashMap::new(),
            boards: DashMap::new(),
            columns: DashMap::new(),
            tasks: DashMap::new(),
            dependencies: DashMap::new(),
            comments: DashMap::new(),
            watchers: DashMap::new(),
            attachments: DashMap::new(),
            milestones: DashMap::new(),
            sprints: DashMap::new(),
            phases: DashMap::new(),
            time_entries: DashMap::new(),
            automation_rules: DashMap::new(),
            templates: DashMap::new(),
            cycles: DashMap::new(),
            program_increments: DashMap::new(),
            business_cases: DashMap::new(),
            activity: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Send a project notification through the Notifications module.
    /// The existing `notification.requested` event is the only channel
    /// the notifications module listens to for ad-hoc messages.
    fn notify(&self, user_id: &str, title: String, body: String) {
        if user_id.trim().is_empty() {
            return;
        }
        self.emit(
            "notification.requested",
            json!({
                "user_id": user_id.trim(),
                "title": title,
                "body": body,
                "kind": "project",
            }),
        );
    }

    /// Ask the Roles module whether `role` holds `permission`
    /// (`role.check`). Fails closed: no answer means not allowed.
    fn check_capability(&self, role: &str, permission: &str) -> bool {
        let context = self.bus().emit(
            "role.check",
            json!({ "role": role, "permission": permission, "allowed": false }),
        );
        context
            .get("allowed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    fn is_member(&self, project_id: &str, user_id: &str) -> bool {
        self.members
            .iter()
            .any(|entry| entry.value().project_id == project_id && entry.value().user_id == user_id)
    }

    fn get_project(&self, id: &str) -> UnifiedResult<Project> {
        self.projects
            .get(id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Project not found"))
    }

    fn get_task(&self, id: &str) -> UnifiedResult<ProjectTask> {
        self.tasks
            .get(id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Task not found"))
    }

    fn log_activity(
        &self,
        project_id: &str,
        actor_id: &str,
        action: &str,
        target_type: &str,
        target_id: Option<String>,
        details: Value,
    ) {
        let record = ProjectActivity {
            id: new_id(),
            project_id: project_id.to_string(),
            actor_id: actor_id.to_string(),
            action: action.to_string(),
            target_type: target_type.to_string(),
            target_id,
            details,
            created_at: now(),
        };
        self.activity.insert(record.id.clone(), record);
    }

    /// Notify a task's assignee plus every watcher, each user once.
    fn notify_task_watchers(&self, task: &ProjectTask, title: String, body: String) {
        let mut recipients: HashSet<String> = HashSet::new();
        if let Some(assignee_id) = &task.assignee_id {
            if !assignee_id.trim().is_empty() {
                recipients.insert(assignee_id.trim().to_string());
            }
        }
        for entry in self.watchers.iter() {
            if entry.value().task_id == task.id && !entry.value().user_id.trim().is_empty() {
                recipients.insert(entry.value().user_id.trim().to_string());
            }
        }
        for user_id in recipients {
            self.notify(&user_id, title.clone(), body.clone());
        }
    }
}

impl Default for ProjectsState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

/// Slice one page out of a filtered, sorted list, mirroring the
/// tickets module: `core::pagination` carries the parameter and
/// result types only, so the slicing helper lives here.
fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

/// Resolve the requester's identity from the list query parameters.
/// Returns (viewer id, viewer role); either side may be absent.
fn viewer_of(params: &PaginationParams) -> (Option<String>, Option<String>) {
    let viewer_id = params
        .filters
        .get("viewer_id")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let viewer_role = params
        .filters
        .get("viewer_role")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    (viewer_id, viewer_role)
}

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

/// Fail when no role was supplied or the role lacks the permission.
fn require_capability(
    state: &ProjectsState,
    role: Option<&str>,
    permission: &str,
) -> UnifiedResult<()> {
    match role {
        Some(role) if state.check_capability(role, permission) => Ok(()),
        _ => Err(UnifiedError::forbidden(format!(
            "Role lacks required capability: {permission}"
        ))),
    }
}

/// A viewer may see a project when they own it, are a member of it,
/// or hold the `projects:view` capability. Anything else is forbidden.
fn require_project_access(
    state: &ProjectsState,
    project: &Project,
    viewer_id: Option<&str>,
    viewer_role: Option<&str>,
) -> UnifiedResult<()> {
    if let Some(viewer_id) = viewer_id {
        if project.owner_id == viewer_id || state.is_member(&project.id, viewer_id) {
            return Ok(());
        }
    }
    if let Some(role) = viewer_role {
        if state.check_capability(role, "projects:view") {
            return Ok(());
        }
    }
    Err(UnifiedError::forbidden(
        "Viewer has no access to this project",
    ))
}

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

/// Create a task from a support ticket (`project.task_from_ticket_requested`).
/// No default project is created: a missing or unknown `project_id`
/// is reported back as an error result.
fn on_task_from_ticket_requested(state: Arc<ProjectsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let project_id = data
        .get("project_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if project_id.is_empty() {
        ctx.add_result(json!({ "error": "project_id is required" }));
        return;
    }
    let project = match state.get_project(&project_id) {
        Ok(project) => project,
        Err(error) => {
            ctx.add_result(json!({ "error": error.message }));
            return;
        }
    };
    let title = data
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let title = if title.is_empty() {
        "Untitled task".to_string()
    } else {
        title
    };
    let body = data
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let user_id = data
        .get("user_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let status = methodology_profile(&project.methodology)
        .and_then(|profile| profile.default_statuses.first().copied())
        .unwrap_or("backlog")
        .to_string();
    let ticket_id = data
        .get("ticket_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let task = ProjectTask {
        id: new_id(),
        project_id: project.id.clone(),
        board_id: None,
        parent_id: None,
        epic_id: None,
        title,
        description: body,
        task_type: "task".to_string(),
        status,
        priority: "normal".to_string(),
        assignee_id: None,
        reporter_id: user_id.clone(),
        estimate_points: None,
        estimate_hours: None,
        labels: Vec::new(),
        custom_values: Value::Object(Default::default()),
        start_date: None,
        due_date: None,
        completed_at: None,
        sprint_id: None,
        phase_id: None,
        cycle_id: None,
        pi_id: None,
        milestone_id: None,
        process_group: None,
        knowledge_area: None,
        waste_flag: None,
        pair_partner_id: None,
        test_first: false,
        dmaic_phase: None,
        appetite_weeks: None,
        created_at: now(),
        updated_at: now(),
    };
    state.tasks.insert(task.id.clone(), task.clone());
    state.log_activity(
        &project.id,
        &user_id,
        "task.created_from_ticket",
        "task",
        Some(task.id.clone()),
        json!({ "ticket_id": ticket_id }),
    );
    let value = serde_json::to_value(&task).unwrap_or(Value::Null);
    state.emit("project.task.created", value);
    ctx.add_result(json!({ "task_id": task.id, "project_id": project.id }));
}

/// Ask the Tickets module to open a ticket for a task
/// (`project.ticket_from_task_requested`).
fn on_ticket_from_task_requested(state: Arc<ProjectsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let task_id = data
        .get("task_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if task_id.is_empty() {
        ctx.add_result(json!({ "error": "task_id is required" }));
        return;
    }
    let task = match state.get_task(&task_id) {
        Ok(task) => task,
        Err(error) => {
            ctx.add_result(json!({ "error": error.message }));
            return;
        }
    };
    state.emit(
        "ticket.create_requested",
        json!({
            "title": task.title,
            "body": task.description,
            "user_id": task.reporter_id,
            "category": "project-task",
            "task_id": task.id,
        }),
    );
    ctx.add_result(json!({ "requested": true, "task_id": task.id }));
}

/// Answer the billing module with unbilled billable hours
/// (`project.billable_hours_requested`). Both filters are optional.
fn on_billable_hours_requested(state: Arc<ProjectsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let project_id = data
        .get("project_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let user_id = data
        .get("user_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let mut hours: f64 = 0.0;
    let mut entries: Vec<Value> = Vec::new();
    for entry in state.time_entries.iter() {
        let record = entry.value();
        if !record.billable || record.billed {
            continue;
        }
        if let Some(project_id) = &project_id {
            if &record.project_id != project_id {
                continue;
            }
        }
        if let Some(user_id) = &user_id {
            if &record.user_id != user_id {
                continue;
            }
        }
        hours += record.hours;
        entries.push(json!({
            "entry_id": record.id,
            "task_id": record.task_id,
            "hours": record.hours,
        }));
    }
    ctx.add_result(json!({
        "project_id": project_id,
        "user_id": user_id,
        "hours": hours,
        "entries": entries,
    }));
}

/// Answer the supervision module's project lookup
/// (`project.get_requested`). A missing project answers
/// `{"exists": false}` — never silence, or supervision fails
/// closed with an internal error instead of a clean "not found".
fn on_project_get_requested(state: Arc<ProjectsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let project_id = data
        .get("project_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if project_id.is_empty() {
        ctx.add_result(json!({ "error": "project_id is required" }));
        return;
    }
    match state.get_project(&project_id) {
        Ok(project) => {
            ctx.add_result(json!({
                "exists": true,
                "project": {
                    "id": project.id,
                    "name": project.name,
                    "status": project.status,
                    "archived": project.archived,
                },
            }));
        }
        Err(_) => {
            ctx.add_result(json!({ "exists": false, "project_id": project_id }));
        }
    }
}

/// Answer the supervision module's progress query
/// (`project.progress_requested`). The percent is derived live
/// from this module's own tasks: completed tasks / total tasks *
/// 100 (0 tasks -> 0.0), rounded to two decimals. With a
/// `milestone_id`, only that milestone's tasks count, and
/// `milestone_reached` comes from the milestone's own `reached`
/// flag; without one, the whole project counts and
/// `milestone_reached` is false. An unknown project or an unknown
/// milestone answers an error — supervision records the snapshot
/// as unverified rather than inventing a number.
fn on_project_progress_requested(state: Arc<ProjectsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let project_id = data
        .get("project_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if project_id.is_empty() {
        ctx.add_result(json!({ "error": "project_id is required" }));
        return;
    }
    if state.get_project(&project_id).is_err() {
        ctx.add_result(json!({ "error": "Project not found" }));
        return;
    }
    let milestone_id = data
        .get("milestone_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let mut milestone_reached = false;
    if let Some(milestone_id) = &milestone_id {
        match state.milestones.get(milestone_id) {
            Some(entry) if entry.value().project_id == project_id => {
                milestone_reached = entry.value().reached;
            }
            _ => {
                ctx.add_result(json!({ "error": "Milestone not found" }));
                return;
            }
        }
    }
    let mut total = 0usize;
    let mut done = 0usize;
    for entry in state.tasks.iter() {
        let task = entry.value();
        if task.project_id != project_id {
            continue;
        }
        if let Some(milestone_id) = &milestone_id {
            if task.milestone_id.as_deref() != Some(milestone_id.as_str()) {
                continue;
            }
        }
        total += 1;
        if s2_task_is_done(&task.status) {
            done += 1;
        }
    }
    let percent = if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64 * 100.0 * 100.0).round() / 100.0
    };
    ctx.add_result(json!({
        "project_id": project_id,
        "milestone_id": milestone_id,
        "percent_complete": percent,
        "milestone_reached": milestone_reached,
    }));
}

/// Create a follow-up task for supervision
/// (`project.task.create_requested`). Mirrors the ticket
/// follow-up shape (default status from the project's
/// methodology profile, `reporter_id` = `requested_by`), with
/// the supervision trace kept in `custom_values` (`source`,
/// `inspection_id`, `defect_id`). An unknown project answers
/// `{"created": false, "error": ...}` — supervision records the
/// follow-up as requested-but-not-created, never as created.
fn on_project_task_create_requested(state: Arc<ProjectsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let project_id = data
        .get("project_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if project_id.is_empty() {
        ctx.add_result(json!({ "created": false, "error": "project_id is required" }));
        return;
    }
    let project = match state.get_project(&project_id) {
        Ok(project) => project,
        Err(error) => {
            ctx.add_result(json!({ "created": false, "error": error.message }));
            return;
        }
    };
    let title = data
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let title = if title.is_empty() {
        "Untitled task".to_string()
    } else {
        title
    };
    let description = data
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let requested_by = data
        .get("requested_by")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let status = methodology_profile(&project.methodology)
        .and_then(|profile| profile.default_statuses.first().copied())
        .unwrap_or("backlog")
        .to_string();
    let mut trace = serde_json::Map::new();
    trace.insert(
        "source".to_string(),
        Value::String("supervision".to_string()),
    );
    if let Some(value) = data.get("inspection_id") {
        trace.insert("inspection_id".to_string(), value.clone());
    }
    if let Some(value) = data.get("defect_id") {
        trace.insert("defect_id".to_string(), value.clone());
    }
    let task = ProjectTask {
        id: new_id(),
        project_id: project.id.clone(),
        board_id: None,
        parent_id: None,
        epic_id: None,
        title,
        description,
        task_type: "task".to_string(),
        status,
        priority: "normal".to_string(),
        assignee_id: None,
        reporter_id: requested_by.clone(),
        estimate_points: None,
        estimate_hours: None,
        labels: Vec::new(),
        custom_values: Value::Object(trace),
        start_date: None,
        due_date: None,
        completed_at: None,
        sprint_id: None,
        phase_id: None,
        cycle_id: None,
        pi_id: None,
        milestone_id: None,
        process_group: None,
        knowledge_area: None,
        waste_flag: None,
        pair_partner_id: None,
        test_first: false,
        dmaic_phase: None,
        appetite_weeks: None,
        created_at: now(),
        updated_at: now(),
    };
    state.tasks.insert(task.id.clone(), task.clone());
    state.log_activity(
        &project.id,
        &requested_by,
        "task.created_from_supervision",
        "task",
        Some(task.id.clone()),
        json!({ "inspection_id": data.get("inspection_id"), "defect_id": data.get("defect_id") }),
    );
    let value = serde_json::to_value(&task).unwrap_or(Value::Null);
    state.emit("project.task.created", value);
    ctx.add_result(json!({ "created": true, "task_id": task.id, "project_id": project.id }));
}

/// Answer the supervision module's task lookup
/// (`project.task.get_requested`). Used to validate defect task
/// links. A missing task answers `{"exists": false}`.
fn on_project_task_get_requested(state: Arc<ProjectsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let task_id = data
        .get("task_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if task_id.is_empty() {
        ctx.add_result(json!({ "error": "task_id is required" }));
        return;
    }
    match state.get_task(&task_id) {
        Ok(task) => {
            let value = serde_json::to_value(&task).unwrap_or(Value::Null);
            ctx.add_result(json!({ "exists": true, "task": value }));
        }
        Err(_) => {
            ctx.add_result(json!({ "exists": false, "task_id": task_id }));
        }
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct ProjectsModule {
    state: Arc<ProjectsState>,
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
        vec!["tickets", "crm", "notifications", "billing", "storage", "roles"]
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
            Arc::new(move |ctx: &mut HookContext| on_task_from_ticket_requested(state.clone(), ctx));
        bus.register("project.task_from_ticket_requested", "projects", handler);
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_ticket_from_task_requested(state.clone(), ctx)
        });
        bus.register("project.ticket_from_task_requested", "projects", handler);
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_billable_hours_requested(state.clone(), ctx)
        });
        bus.register("project.billable_hours_requested", "projects", handler);
        // Supervision query hooks (batch 5 contract): project
        // lookup, live progress, follow-up task creation, and task
        // lookup. Supervision owns no project data and fails closed
        // until these answer.
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_project_get_requested(state.clone(), ctx)
        });
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
                .route("/{id}/members/{user_id}", web::put().to(update_project_member))
                .route("/{id}/members/{user_id}", web::delete().to(remove_project_member))
                .route("/{id}/apply-template", web::post().to(apply_project_template))
                .route("/{id}/boards", web::get().to(list_project_boards))
                .route("/{id}/boards", web::post().to(create_project_board))
                .route("/{id}/boards/{board_id}", web::get().to(get_project_board))
                .route("/{id}/boards/{board_id}", web::put().to(update_project_board))
                .route("/{id}/boards/{board_id}", web::delete().to(delete_project_board))
                .route("/{id}/boards/{board_id}/columns", web::get().to(list_board_columns))
                .route("/{id}/boards/{board_id}/columns", web::post().to(create_board_column))
                .route("/{id}/boards/{board_id}/columns/{column_id}", web::put().to(update_board_column))
                .route("/{id}/boards/{board_id}/columns/{column_id}", web::delete().to(delete_board_column))
                .route("/{id}/milestones", web::get().to(list_project_milestones))
                .route("/{id}/milestones", web::post().to(create_project_milestone))
                .route("/{id}/milestones/{milestone_id}", web::get().to(get_project_milestone))
                .route("/{id}/milestones/{milestone_id}", web::put().to(update_project_milestone))
                .route("/{id}/milestones/{milestone_id}", web::delete().to(delete_project_milestone))
                .route("/{id}/milestones/{milestone_id}/reach", web::post().to(reach_project_milestone))
                .route("/{id}/tasks", web::get().to(list_project_tasks))
                .route("/{id}/tasks", web::post().to(create_project_task))
                // Literal `from-ticket` must win over `/{task_id}`.
                .route("/{id}/tasks/from-ticket", web::post().to(create_task_from_ticket))
                .route("/{id}/tasks/{task_id}", web::get().to(get_project_task))
                .route("/{id}/tasks/{task_id}", web::put().to(update_project_task))
                .route("/{id}/tasks/{task_id}", web::delete().to(delete_project_task))
                .route("/{id}/tasks/{task_id}/move", web::post().to(move_project_task))
                .route("/{id}/tasks/{task_id}/assign", web::post().to(assign_project_task))
                .route("/{id}/tasks/{task_id}/complete", web::post().to(complete_project_task))
                .route("/{id}/tasks/{task_id}/reopen", web::post().to(reopen_project_task))
                .route("/{id}/tasks/{task_id}/subtasks", web::get().to(list_task_subtasks))
                .route("/{id}/tasks/{task_id}/dependencies", web::get().to(list_task_dependencies))
                .route("/{id}/tasks/{task_id}/dependencies", web::post().to(add_task_dependency))
                .route("/{id}/tasks/{task_id}/dependencies/{dependency_id}", web::delete().to(remove_task_dependency))
                .route("/{id}/tasks/{task_id}/comments", web::get().to(list_task_comments))
                .route("/{id}/tasks/{task_id}/comments", web::post().to(add_task_comment))
                .route("/{id}/tasks/{task_id}/comments/{comment_id}", web::put().to(update_task_comment))
                .route("/{id}/tasks/{task_id}/comments/{comment_id}", web::delete().to(delete_task_comment))
                .route("/{id}/tasks/{task_id}/watchers", web::get().to(list_task_watchers))
                .route("/{id}/tasks/{task_id}/watchers", web::post().to(add_task_watcher))
                .route("/{id}/tasks/{task_id}/watchers/{user_id}", web::delete().to(remove_task_watcher))
                .route("/{id}/tasks/{task_id}/attachments", web::get().to(list_task_attachments))
                .route("/{id}/tasks/{task_id}/attachments", web::post().to(add_task_attachment))
                .route("/{id}/tasks/{task_id}/attachments/{attachment_id}", web::delete().to(delete_task_attachment))
                .route("/{id}/tasks/{task_id}/convert-to-ticket", web::post().to(convert_task_to_ticket))
                .route("/{id}/automation-rules", web::get().to(list_automation_rules))
                .route("/{id}/automation-rules", web::post().to(create_automation_rule))
                .route("/{id}/automation-rules/{rule_id}", web::get().to(get_automation_rule))
                .route("/{id}/automation-rules/{rule_id}", web::put().to(update_automation_rule))
                .route("/{id}/automation-rules/{rule_id}", web::delete().to(delete_automation_rule))
                .route("/{id}/automation-rules/{rule_id}/run", web::post().to(run_automation_rule))
                .route("/{id}/sprints", web::get().to(list_project_sprints))
                .route("/{id}/sprints", web::post().to(create_project_sprint))
                .route("/{id}/sprints/{sprint_id}", web::get().to(get_project_sprint))
                .route("/{id}/sprints/{sprint_id}", web::put().to(update_project_sprint))
                .route("/{id}/sprints/{sprint_id}", web::delete().to(delete_project_sprint))
                .route("/{id}/sprints/{sprint_id}/start", web::post().to(start_project_sprint))
                .route("/{id}/sprints/{sprint_id}/close", web::post().to(close_project_sprint))
                .route("/{id}/sprints/{sprint_id}/standup", web::post().to(add_sprint_standup))
                .route("/{id}/sprints/{sprint_id}/review", web::post().to(save_sprint_review))
                .route("/{id}/sprints/{sprint_id}/retrospective", web::post().to(save_sprint_retrospective))
                .route("/{id}/phases", web::get().to(list_project_phases))
                .route("/{id}/phases", web::post().to(create_project_phase))
                .route("/{id}/phases/{phase_id}", web::get().to(get_project_phase))
                .route("/{id}/phases/{phase_id}", web::put().to(update_project_phase))
                .route("/{id}/phases/{phase_id}", web::delete().to(delete_project_phase))
                .route("/{id}/phases/{phase_id}/gate", web::post().to(decide_phase_gate))
                .route("/{id}/phases/{phase_id}/complete", web::post().to(complete_project_phase))
                .route("/{id}/cycles", web::get().to(list_project_cycles))
                .route("/{id}/cycles", web::post().to(create_project_cycle))
                .route("/{id}/cycles/{cycle_id}", web::get().to(get_project_cycle))
                .route("/{id}/cycles/{cycle_id}", web::put().to(update_project_cycle))
                .route("/{id}/cycles/{cycle_id}", web::delete().to(delete_project_cycle))
                .route("/{id}/cycles/{cycle_id}/bet", web::post().to(bet_project_cycle))
                .route("/{id}/cycles/{cycle_id}/complete", web::post().to(complete_project_cycle))
                .route("/{id}/program-increments", web::get().to(list_program_increments))
                .route("/{id}/program-increments", web::post().to(create_program_increment))
                .route("/{id}/program-increments/{pi_id}", web::get().to(get_program_increment))
                .route("/{id}/program-increments/{pi_id}", web::put().to(update_program_increment))
                .route("/{id}/program-increments/{pi_id}", web::delete().to(delete_program_increment))
                .route("/{id}/program-increments/{pi_id}/plan", web::post().to(plan_program_increment))
                .route("/{id}/business-cases", web::get().to(list_business_cases))
                .route("/{id}/business-cases", web::post().to(create_business_case))
                .route("/{id}/business-cases/{case_id}", web::get().to(get_business_case))
                .route("/{id}/business-cases/{case_id}", web::put().to(update_business_case))
                .route("/{id}/business-cases/{case_id}", web::delete().to(delete_business_case))
                .route("/{id}/business-cases/{case_id}/exception-report", web::post().to(save_exception_report))
                .route("/{id}/time-entries", web::get().to(list_time_entries))
                .route("/{id}/time-entries", web::post().to(create_time_entry))
                // Literal `billable` must win over `/{entry_id}`.
                .route("/{id}/time-entries/billable", web::get().to(list_billable_time))
                .route("/{id}/time-entries/{entry_id}", web::get().to(get_time_entry))
                .route("/{id}/time-entries/{entry_id}", web::put().to(update_time_entry))
                .route("/{id}/time-entries/{entry_id}", web::delete().to(delete_time_entry))
                .route("/{id}/time-entries/{entry_id}/start-timer", web::post().to(start_time_timer))
                .route("/{id}/time-entries/{entry_id}/stop-timer", web::post().to(stop_time_timer))
                .route("/{id}/workload", web::get().to(get_project_workload))
                .route("/{id}/gantt", web::get().to(get_project_gantt))
                .route("/{id}/timeline", web::get().to(get_project_timeline))
                .route("/{id}/calendar", web::get().to(get_project_calendar))
                .route("/{id}/reports/burndown", web::get().to(get_burndown_report))
                .route("/{id}/reports/velocity", web::get().to(get_velocity_report))
                .route("/{id}/reports/cumulative-flow", web::get().to(get_cumulative_flow_report))
                .route("/{id}/reports/cycle-time", web::get().to(get_cycle_time_report))
                .route("/{id}/reports/critical-path", web::get().to(get_critical_path_report))
                .route("/{id}/export", web::get().to(export_project))
                .route("/{id}/import", web::post().to(import_project))
                .route("/{id}/activity", web::get().to(list_project_activity)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/projects", owner: "projects", description: "List projects (paginated; viewer-scoped)." },
            RouteInfo { method: "POST", path: "/api/v1/projects", owner: "projects", description: "Create a project with a methodology." },
            RouteInfo { method: "GET", path: "/api/v1/projects/methodologies", owner: "projects", description: "List the 15 supported methodologies with their defaults." },
            RouteInfo { method: "GET", path: "/api/v1/projects/templates", owner: "projects", description: "List project templates (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/projects/templates", owner: "projects", description: "Create a project template." },
            RouteInfo { method: "GET", path: "/api/v1/projects/templates/{id}", owner: "projects", description: "Get one project template." },
            RouteInfo { method: "PUT", path: "/api/v1/projects/templates/{id}", owner: "projects", description: "Update a project template." },
            RouteInfo { method: "DELETE", path: "/api/v1/projects/templates/{id}", owner: "projects", description: "Delete a project template." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}", owner: "projects", description: "Get one project." },
            RouteInfo { method: "PUT", path: "/api/v1/projects/{id}", owner: "projects", description: "Update a project." },
            RouteInfo { method: "DELETE", path: "/api/v1/projects/{id}", owner: "projects", description: "Delete a project." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/archive", owner: "projects", description: "Archive a project." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/restore", owner: "projects", description: "Restore an archived project." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/overview", owner: "projects", description: "Get a project overview (counts and progress)." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/members", owner: "projects", description: "List project members." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/members", owner: "projects", description: "Add a member to a project." },
            RouteInfo { method: "PUT", path: "/api/v1/projects/{id}/members/{user_id}", owner: "projects", description: "Update a project member's role." },
            RouteInfo { method: "DELETE", path: "/api/v1/projects/{id}/members/{user_id}", owner: "projects", description: "Remove a member from a project." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/apply-template", owner: "projects", description: "Apply a template (boards, columns, phases, custom fields) to a project." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/boards", owner: "projects", description: "List project boards." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/boards", owner: "projects", description: "Create a project board." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/boards/{board_id}", owner: "projects", description: "Get one project board." },
            RouteInfo { method: "PUT", path: "/api/v1/projects/{id}/boards/{board_id}", owner: "projects", description: "Update a project board." },
            RouteInfo { method: "DELETE", path: "/api/v1/projects/{id}/boards/{board_id}", owner: "projects", description: "Delete a project board." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/boards/{board_id}/columns", owner: "projects", description: "List board columns." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/boards/{board_id}/columns", owner: "projects", description: "Create a board column." },
            RouteInfo { method: "PUT", path: "/api/v1/projects/{id}/boards/{board_id}/columns/{column_id}", owner: "projects", description: "Update a board column." },
            RouteInfo { method: "DELETE", path: "/api/v1/projects/{id}/boards/{board_id}/columns/{column_id}", owner: "projects", description: "Delete a board column." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/milestones", owner: "projects", description: "List project milestones." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/milestones", owner: "projects", description: "Create a project milestone." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/milestones/{milestone_id}", owner: "projects", description: "Get one project milestone." },
            RouteInfo { method: "PUT", path: "/api/v1/projects/{id}/milestones/{milestone_id}", owner: "projects", description: "Update a project milestone." },
            RouteInfo { method: "DELETE", path: "/api/v1/projects/{id}/milestones/{milestone_id}", owner: "projects", description: "Delete a project milestone." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/milestones/{milestone_id}/reach", owner: "projects", description: "Mark a milestone as reached." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/tasks", owner: "projects", description: "List project tasks (paginated; filter by status, assignee, sprint, phase, labels)." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/tasks", owner: "projects", description: "Create a project task." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/tasks/{task_id}", owner: "projects", description: "Get one project task." },
            RouteInfo { method: "PUT", path: "/api/v1/projects/{id}/tasks/{task_id}", owner: "projects", description: "Update a project task." },
            RouteInfo { method: "DELETE", path: "/api/v1/projects/{id}/tasks/{task_id}", owner: "projects", description: "Delete a project task." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/tasks/{task_id}/move", owner: "projects", description: "Move a task to another board, column, or status." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/tasks/{task_id}/assign", owner: "projects", description: "Assign a task to a user." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/tasks/{task_id}/complete", owner: "projects", description: "Mark a task as completed." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/tasks/{task_id}/reopen", owner: "projects", description: "Reopen a completed task." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/tasks/{task_id}/subtasks", owner: "projects", description: "List a task's subtasks." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/tasks/{task_id}/dependencies", owner: "projects", description: "List a task's dependencies." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/tasks/{task_id}/dependencies", owner: "projects", description: "Add a dependency to a task." },
            RouteInfo { method: "DELETE", path: "/api/v1/projects/{id}/tasks/{task_id}/dependencies/{dependency_id}", owner: "projects", description: "Remove a dependency from a task." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/tasks/{task_id}/comments", owner: "projects", description: "List a task's comments." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/tasks/{task_id}/comments", owner: "projects", description: "Add a comment to a task." },
            RouteInfo { method: "PUT", path: "/api/v1/projects/{id}/tasks/{task_id}/comments/{comment_id}", owner: "projects", description: "Update a task comment." },
            RouteInfo { method: "DELETE", path: "/api/v1/projects/{id}/tasks/{task_id}/comments/{comment_id}", owner: "projects", description: "Delete a task comment." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/tasks/{task_id}/watchers", owner: "projects", description: "List a task's watchers." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/tasks/{task_id}/watchers", owner: "projects", description: "Watch a task." },
            RouteInfo { method: "DELETE", path: "/api/v1/projects/{id}/tasks/{task_id}/watchers/{user_id}", owner: "projects", description: "Stop watching a task." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/tasks/{task_id}/attachments", owner: "projects", description: "List a task's attachments (storage references)." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/tasks/{task_id}/attachments", owner: "projects", description: "Attach a file already uploaded through the Storage module." },
            RouteInfo { method: "DELETE", path: "/api/v1/projects/{id}/tasks/{task_id}/attachments/{attachment_id}", owner: "projects", description: "Delete a task attachment reference." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/automation-rules", owner: "projects", description: "List project automation rules." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/automation-rules", owner: "projects", description: "Create a project automation rule." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/automation-rules/{rule_id}", owner: "projects", description: "Get one automation rule." },
            RouteInfo { method: "PUT", path: "/api/v1/projects/{id}/automation-rules/{rule_id}", owner: "projects", description: "Update an automation rule." },
            RouteInfo { method: "DELETE", path: "/api/v1/projects/{id}/automation-rules/{rule_id}", owner: "projects", description: "Delete an automation rule." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/automation-rules/{rule_id}/run", owner: "projects", description: "Run an automation rule now." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/tasks/{task_id}/convert-to-ticket", owner: "projects", description: "Convert a task into a support ticket (hook)." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/tasks/from-ticket", owner: "projects", description: "Create a task from a support ticket (hook)." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/sprints", owner: "projects", description: "List project sprints." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/sprints", owner: "projects", description: "Create a project sprint." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/sprints/{sprint_id}", owner: "projects", description: "Get one project sprint." },
            RouteInfo { method: "PUT", path: "/api/v1/projects/{id}/sprints/{sprint_id}", owner: "projects", description: "Update a project sprint." },
            RouteInfo { method: "DELETE", path: "/api/v1/projects/{id}/sprints/{sprint_id}", owner: "projects", description: "Delete a project sprint." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/sprints/{sprint_id}/start", owner: "projects", description: "Start a sprint." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/sprints/{sprint_id}/close", owner: "projects", description: "Close a sprint and record its velocity." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/sprints/{sprint_id}/standup", owner: "projects", description: "Add a standup note to a sprint." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/sprints/{sprint_id}/review", owner: "projects", description: "Save a sprint review." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/sprints/{sprint_id}/retrospective", owner: "projects", description: "Save a sprint retrospective." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/phases", owner: "projects", description: "List project phases." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/phases", owner: "projects", description: "Create a project phase." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/phases/{phase_id}", owner: "projects", description: "Get one project phase." },
            RouteInfo { method: "PUT", path: "/api/v1/projects/{id}/phases/{phase_id}", owner: "projects", description: "Update a project phase." },
            RouteInfo { method: "DELETE", path: "/api/v1/projects/{id}/phases/{phase_id}", owner: "projects", description: "Delete a project phase." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/phases/{phase_id}/gate", owner: "projects", description: "Decide a phase gate (approve or reject)." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/phases/{phase_id}/complete", owner: "projects", description: "Complete a project phase." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/cycles", owner: "projects", description: "List Shape Up cycles." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/cycles", owner: "projects", description: "Create a Shape Up cycle." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/cycles/{cycle_id}", owner: "projects", description: "Get one Shape Up cycle." },
            RouteInfo { method: "PUT", path: "/api/v1/projects/{id}/cycles/{cycle_id}", owner: "projects", description: "Update a Shape Up cycle." },
            RouteInfo { method: "DELETE", path: "/api/v1/projects/{id}/cycles/{cycle_id}", owner: "projects", description: "Delete a Shape Up cycle." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/cycles/{cycle_id}/bet", owner: "projects", description: "Record a betting decision for a cycle." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/cycles/{cycle_id}/complete", owner: "projects", description: "Complete a Shape Up cycle." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/program-increments", owner: "projects", description: "List SAFe program increments." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/program-increments", owner: "projects", description: "Create a program increment." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/program-increments/{pi_id}", owner: "projects", description: "Get one program increment." },
            RouteInfo { method: "PUT", path: "/api/v1/projects/{id}/program-increments/{pi_id}", owner: "projects", description: "Update a program increment." },
            RouteInfo { method: "DELETE", path: "/api/v1/projects/{id}/program-increments/{pi_id}", owner: "projects", description: "Delete a program increment." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/program-increments/{pi_id}/plan", owner: "projects", description: "Plan a program increment (objectives)." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/business-cases", owner: "projects", description: "List PRINCE2 business cases." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/business-cases", owner: "projects", description: "Create a business case." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/business-cases/{case_id}", owner: "projects", description: "Get one business case." },
            RouteInfo { method: "PUT", path: "/api/v1/projects/{id}/business-cases/{case_id}", owner: "projects", description: "Update a business case." },
            RouteInfo { method: "DELETE", path: "/api/v1/projects/{id}/business-cases/{case_id}", owner: "projects", description: "Delete a business case." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/business-cases/{case_id}/exception-report", owner: "projects", description: "Save an exception report for a business case." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/time-entries", owner: "projects", description: "List project time entries." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/time-entries", owner: "projects", description: "Create a time entry." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/time-entries/billable", owner: "projects", description: "List unbilled billable time entries." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/time-entries/{entry_id}", owner: "projects", description: "Get one time entry." },
            RouteInfo { method: "PUT", path: "/api/v1/projects/{id}/time-entries/{entry_id}", owner: "projects", description: "Update a time entry." },
            RouteInfo { method: "DELETE", path: "/api/v1/projects/{id}/time-entries/{entry_id}", owner: "projects", description: "Delete a time entry." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/time-entries/{entry_id}/start-timer", owner: "projects", description: "Start a running timer on a time entry." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/time-entries/{entry_id}/stop-timer", owner: "projects", description: "Stop a running timer and record its hours." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/workload", owner: "projects", description: "Get per-member workload for a project." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/gantt", owner: "projects", description: "Get a project's Gantt view (tasks and dependencies)." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/timeline", owner: "projects", description: "Get a project's timeline view." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/calendar", owner: "projects", description: "Get a project's calendar view." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/reports/burndown", owner: "projects", description: "Get the sprint burndown report." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/reports/velocity", owner: "projects", description: "Get the velocity report." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/reports/cumulative-flow", owner: "projects", description: "Get the cumulative flow report." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/reports/cycle-time", owner: "projects", description: "Get the cycle time report." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/reports/critical-path", owner: "projects", description: "Get the critical path report." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/export", owner: "projects", description: "Export a project (tasks, boards, sprints, phases, time entries)." },
            RouteInfo { method: "POST", path: "/api/v1/projects/{id}/import", owner: "projects", description: "Import a project export." },
            RouteInfo { method: "GET", path: "/api/v1/projects/{id}/activity", owner: "projects", description: "List a project's activity feed." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ProjectsModule::new()));
}

// Section s2 of the Projects module: projects, members, methodologies,
// templates, boards, columns, and milestones.
//
// This file is assembled into `src/modules/projects.rs` after s1. It
// defines no imports, no module header, no entity structs, and no
// `Module` implementation; those live in s1. Every helper defined here
// is prefixed with `s2_` so it cannot collide with other sections.
//
// Viewer identity for every handler comes from the query string
// (`viewer_id` / `viewer_role` filters) through the s1 helper
// `viewer_of`, so even body-carrying handlers take a
// `web::Query<PaginationParams>` extractor.

// ---------------------------------------------------------------------------
// Input structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProject {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub methodology: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub owner_id: Option<String>,
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub start_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub due_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    #[serde(default)]
    pub custom_field_definitions: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProject {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub methodology: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub start_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub due_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    #[serde(default)]
    pub custom_field_definitions: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddProjectMember {
    pub user_id: String,
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProjectMember {
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProjectTemplate {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub methodology: Option<String>,
    #[serde(default)]
    pub boards: Option<Value>,
    #[serde(default)]
    pub columns: Option<Value>,
    #[serde(default)]
    pub statuses: Option<Value>,
    #[serde(default)]
    pub phases: Option<Value>,
    #[serde(default)]
    pub custom_fields: Option<Value>,
    #[serde(default)]
    pub is_system: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProjectTemplate {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub methodology: Option<String>,
    #[serde(default)]
    pub boards: Option<Value>,
    #[serde(default)]
    pub columns: Option<Value>,
    #[serde(default)]
    pub statuses: Option<Value>,
    #[serde(default)]
    pub phases: Option<Value>,
    #[serde(default)]
    pub custom_fields: Option<Value>,
    #[serde(default)]
    pub is_system: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApplyTemplate {
    pub template_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProjectBoard {
    pub name: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub wip_limit: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProjectBoard {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub wip_limit: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateBoardColumn {
    pub name: String,
    #[serde(default)]
    pub status_key: Option<String>,
    #[serde(default)]
    pub position: Option<u32>,
    #[serde(default)]
    pub wip_limit: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateBoardColumn {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status_key: Option<String>,
    #[serde(default)]
    pub position: Option<u32>,
    #[serde(default)]
    pub wip_limit: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProjectMilestone {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub due_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProjectMilestone {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub due_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub reached: Option<bool>,
}

// ---------------------------------------------------------------------------
// Private helpers (all prefixed `s2_`)
// ---------------------------------------------------------------------------

/// Normalize a column name (or supplied key) into a status key:
/// trimmed, lowercased, spaces and dashes become underscores.
fn s2_normalize_status_key(input: &str) -> String {
    input
        .trim()
        .to_lowercase()
        .replace(' ', "_")
        .replace('-', "_")
}

fn s2_valid_project_status(status: &str) -> bool {
    matches!(status, "active" | "on_hold" | "completed" | "cancelled")
}

fn s2_valid_visibility(visibility: &str) -> bool {
    matches!(visibility, "private" | "internal" | "public")
}

fn s2_valid_member_role(role: &str) -> bool {
    matches!(role, "owner" | "manager" | "member" | "viewer")
}

/// A task counts as done when its status is `done` or `completed`.
fn s2_task_is_done(status: &str) -> bool {
    matches!(status, "done" | "completed")
}

/// Clean an optional free-text field: trimmed, empty becomes `None`.
fn s2_clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// Fetch a board and confirm it belongs to the given project.
fn s2_get_board(
    state: &ProjectsState,
    project_id: &str,
    board_id: &str,
) -> UnifiedResult<ProjectBoard> {
    let board = state
        .boards
        .get(board_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Project board not found"))?;
    if board.project_id != project_id {
        return Err(UnifiedError::not_found("Project board not found"));
    }
    Ok(board)
}

/// Fetch a column and confirm it belongs to the given board.
fn s2_get_column(
    state: &ProjectsState,
    board_id: &str,
    column_id: &str,
) -> UnifiedResult<ProjectBoardColumn> {
    let column = state
        .columns
        .get(column_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Board column not found"))?;
    if column.board_id != board_id {
        return Err(UnifiedError::not_found("Board column not found"));
    }
    Ok(column)
}

/// Fetch a milestone and confirm it belongs to the given project.
fn s2_get_milestone(
    state: &ProjectsState,
    project_id: &str,
    milestone_id: &str,
) -> UnifiedResult<ProjectMilestone> {
    let milestone = state
        .milestones
        .get(milestone_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Project milestone not found"))?;
    if milestone.project_id != project_id {
        return Err(UnifiedError::not_found("Project milestone not found"));
    }
    Ok(milestone)
}

/// Build a board column from a template JSON value. Returns `None`
/// when the value carries no usable name.
fn s2_column_from_value(
    board_id: &str,
    value: &Value,
    position: u32,
) -> Option<ProjectBoardColumn> {
    let object = value.as_object()?;
    let name = object
        .get("name")
        .and_then(|entry| entry.as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())?;
    let status_key = match object.get("status_key").and_then(|entry| entry.as_str()) {
        Some(key) if !key.trim().is_empty() => s2_normalize_status_key(key),
        _ => s2_normalize_status_key(name),
    };
    let wip_limit = object
        .get("wip_limit")
        .and_then(|entry| entry.as_u64())
        .map(|limit| limit as u32)
        .filter(|limit| *limit > 0);
    Some(ProjectBoardColumn {
        id: new_id(),
        board_id: board_id.to_string(),
        name: name.to_string(),
        position,
        status_key,
        wip_limit,
    })
}

/// Aggregated numbers for one project, returned by
/// `get_project_overview`.
#[derive(Debug, Clone, Serialize)]
pub struct S2ProjectOverview {
    pub project_id: String,
    pub tasks_total: u64,
    pub tasks_open: u64,
    pub tasks_done: u64,
    pub tasks_overdue: u64,
    pub members: u64,
    pub milestones_total: u64,
    pub milestones_reached: u64,
    pub sprints_active: u64,
    pub total_logged_hours: f64,
    pub billable_unbilled_hours: f64,
    pub progress_percent: f64,
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

async fn list_projects(
    state: web::Data<Arc<ProjectsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    // A viewer without the global view capability only sees projects
    // they own or are a member of. No viewer means unscoped.
    let can_view_all = match viewer_role.as_deref() {
        Some(role) => state.check_capability(role, "projects:view"),
        None => false,
    };
    let scoped = viewer_id.is_some() && !can_view_all;
    let sort = params.allowed_sort(&["created_at", "updated_at", "name", "due_date"], "created_at");
    let mut items: Vec<Project> = state
        .projects
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|project| {
            matches_search(&format!("{} {}", project.name, project.description), &params)
                && params
                    .filters
                    .get("methodology")
                    .is_none_or(|value| project.methodology == *value)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|value| project.status == *value)
                && params
                    .filters
                    .get("visibility")
                    .is_none_or(|value| project.visibility == *value)
                && params
                    .filters
                    .get("owner_id")
                    .is_none_or(|value| project.owner_id == *value)
                && params.filters.get("company_id").is_none_or(|value| {
                    project.company_id.as_deref() == Some(value.as_str())
                })
                && params.filters.get("archived").is_none_or(|value| {
                    project.archived == (value == "true")
                })
                && (!scoped
                    || viewer_id.as_ref().is_some_and(|viewer| {
                        project.owner_id == *viewer
                            || state.is_member(&project.id, viewer)
                    }))
        })
        .collect();
    match sort.as_str() {
        "updated_at" => items.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        "due_date" => items.sort_by(|a, b| a.due_date.cmp(&b.due_date)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_project(
    state: web::Data<Arc<ProjectsState>>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateProject>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Project> = (|| {
        require_capability(&state, viewer_role.as_deref(), "projects:create")?;
        let input = body.into_inner();
        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Err(UnifiedError::bad_request("Project name must not be empty"));
        }
        let methodology = match input.methodology {
            Some(value) if valid_methodology(value.trim()) => value.trim().to_string(),
            Some(value) => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown methodology: {}",
                    value.trim()
                )));
            }
            None => {
                return Err(UnifiedError::bad_request(
                    "methodology must not be empty",
                ));
            }
        };
        let status = match input.status {
            Some(value) if s2_valid_project_status(value.trim()) => {
                value.trim().to_string()
            }
            Some(value) => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown project status: {}",
                    value.trim()
                )));
            }
            None => "active".to_string(),
        };
        let visibility = match input.visibility {
            Some(value) if s2_valid_visibility(value.trim()) => value.trim().to_string(),
            Some(value) => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown visibility: {}",
                    value.trim()
                )));
            }
            None => "private".to_string(),
        };
        let start_date = input.start_date;
        let due_date = input.due_date;
        if let (Some(start), Some(due)) = (start_date, due_date) {
            if due < start {
                return Err(UnifiedError::bad_request(
                    "due_date must not be earlier than start_date",
                ));
            }
        }
        let owner_id = match s2_clean_optional(input.owner_id) {
            Some(owner) => owner,
            None => match viewer_id.clone() {
                Some(viewer) if !viewer.trim().is_empty() => viewer.trim().to_string(),
                _ => {
                    return Err(UnifiedError::bad_request(
                        "owner_id must not be empty",
                    ));
                }
            },
        };
        let profile = methodology_profile(&methodology)
            .ok_or_else(|| UnifiedError::bad_request("Unknown methodology"))?;
        let timestamp = now();
        let project = Project {
            id: new_id(),
            name: name.clone(),
            description: s2_clean_optional(input.description).unwrap_or_default(),
            methodology: methodology.clone(),
            status,
            visibility,
            owner_id: owner_id.clone(),
            company_id: s2_clean_optional(input.company_id),
            client_id: s2_clean_optional(input.client_id),
            start_date,
            due_date,
            color: s2_clean_optional(input.color),
            icon: s2_clean_optional(input.icon),
            labels: input
                .labels
                .unwrap_or_default()
                .into_iter()
                .map(|label| label.trim().to_string())
                .filter(|label| !label.is_empty())
                .collect(),
            custom_field_definitions: input
                .custom_field_definitions
                .unwrap_or_else(|| json!({})),
            archived: false,
            created_at: timestamp,
            updated_at: timestamp,
        };
        state.projects.insert(project.id.clone(), project.clone());
        // The owner joins the project as its owner member.
        let owner_member = ProjectMember {
            id: new_id(),
            project_id: project.id.clone(),
            user_id: owner_id.clone(),
            role: "owner".to_string(),
            joined_at: timestamp,
        };
        state
            .members
            .insert(owner_member.id.clone(), owner_member);
        // Default board from the methodology profile, with one column
        // per default column in profile order.
        let board = ProjectBoard {
            id: new_id(),
            project_id: project.id.clone(),
            name: format!("{name} Board"),
            kind: profile.default_board_kind.to_string(),
            wip_limit: None,
            created_at: timestamp,
            updated_at: timestamp,
        };
        state.boards.insert(board.id.clone(), board.clone());
        for (position, column_name) in profile.default_columns.iter().enumerate() {
            let column_name = column_name.to_string();
            let column = ProjectBoardColumn {
                id: new_id(),
                board_id: board.id.clone(),
                name: column_name.clone(),
                position: position as u32,
                status_key: s2_normalize_status_key(&column_name),
                wip_limit: None,
            };
            state.columns.insert(column.id.clone(), column);
        }
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project.id,
            actor,
            "project.created",
            "project",
            Some(project.id.clone()),
            json!({ "name": project.name, "methodology": project.methodology }),
        );
        state.emit(
            "project.created",
            serde_json::to_value(&project).unwrap_or(Value::Null),
        );
        Ok(project)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn get_project(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Project> = (|| {
        let project = state.get_project(path.as_str())?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        Ok(project)
    })();
    json::respond(result, false)
}

async fn update_project(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateProject>,
) -> HttpResponse {
    let project_id = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Project> = (|| {
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        let input = body.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        // A methodology change is only allowed while the project has
        // no tasks yet; existing tasks already follow the old method.
        if let Some(methodology) = input.methodology.as_ref().map(|value| value.trim().to_string())
        {
            if methodology != project.methodology {
                if !valid_methodology(&methodology) {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown methodology: {methodology}"
                    )));
                }
                let has_tasks = state
                    .tasks
                    .iter()
                    .any(|entry| entry.value().project_id == project_id);
                if has_tasks {
                    return Err(UnifiedError::bad_request(
                        "Cannot change methodology while the project has tasks",
                    ));
                }
            }
        }
        let new_start = input.start_date.or(project.start_date);
        let new_due = input.due_date.or(project.due_date);
        if let (Some(start), Some(due)) = (new_start, new_due) {
            if due < start {
                return Err(UnifiedError::bad_request(
                    "due_date must not be earlier than start_date",
                ));
            }
        }
        let mut stored = state
            .projects
            .get_mut(&project_id)
            .ok_or_else(|| UnifiedError::not_found("Project not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request(
                    "Project name must not be empty",
                ));
            }
            stored.name = name.trim().to_string();
        }
        if let Some(description) = input.description {
            stored.description = description.trim().to_string();
        }
        if let Some(methodology) = input.methodology {
            stored.methodology = methodology.trim().to_string();
        }
        if let Some(status) = input.status {
            if !s2_valid_project_status(status.trim()) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown project status: {}",
                    status.trim()
                )));
            }
            stored.status = status.trim().to_string();
        }
        if let Some(visibility) = input.visibility {
            if !s2_valid_visibility(visibility.trim()) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown visibility: {}",
                    visibility.trim()
                )));
            }
            stored.visibility = visibility.trim().to_string();
        }
        if input.start_date.is_some() {
            stored.start_date = input.start_date;
        }
        if input.due_date.is_some() {
            stored.due_date = input.due_date;
        }
        if let Some(color) = input.color {
            stored.color = s2_clean_optional(Some(color));
        }
        if let Some(icon) = input.icon {
            stored.icon = s2_clean_optional(Some(icon));
        }
        if let Some(labels) = input.labels {
            stored.labels = labels
                .into_iter()
                .map(|label| label.trim().to_string())
                .filter(|label| !label.is_empty())
                .collect();
        }
        if let Some(definitions) = input.custom_field_definitions {
            stored.custom_field_definitions = definitions;
        }
        stored.updated_at = now();
        let updated = stored.clone();
        drop(stored);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "project.updated",
            "project",
            Some(project_id.clone()),
            json!({ "name": updated.name }),
        );
        state.emit(
            "project.updated",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_project(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let project_id = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Value> = (|| {
        require_capability(&state, viewer_role.as_deref(), "projects:delete")?;
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        // Boards first: their columns are removed by board id below.
        let board_ids: Vec<String> = state
            .boards
            .iter()
            .filter(|entry| entry.value().project_id == project_id)
            .map(|entry| entry.key().clone())
            .collect();
        // Dependencies, comments, watchers and attachments link to
        // tasks, not to the project, so collect the project's task
        // ids before the tasks themselves are removed.
        let task_ids: std::collections::HashSet<String> = state
            .tasks
            .iter()
            .filter(|entry| entry.value().project_id == project_id)
            .map(|entry| entry.key().clone())
            .collect();
        state.projects.remove(&project_id);
        state.members.retain(|_, member| member.project_id != project_id);
        state.boards.retain(|_, board| board.project_id != project_id);
        state
            .columns
            .retain(|_, column| !board_ids.contains(&column.board_id));
        state.tasks.retain(|_, task| task.project_id != project_id);
        state.dependencies.retain(|_, dependency| {
            !task_ids.contains(&dependency.task_id)
                && !task_ids.contains(&dependency.depends_on_task_id)
        });
        state
            .comments
            .retain(|_, comment| !task_ids.contains(&comment.task_id));
        state
            .watchers
            .retain(|_, watcher| !task_ids.contains(&watcher.task_id));
        state
            .attachments
            .retain(|_, attachment| !task_ids.contains(&attachment.task_id));
        state
            .milestones
            .retain(|_, milestone| milestone.project_id != project_id);
        state.sprints.retain(|_, sprint| sprint.project_id != project_id);
        state.phases.retain(|_, phase| phase.project_id != project_id);
        state
            .time_entries
            .retain(|_, entry| entry.project_id != project_id);
        state
            .automation_rules
            .retain(|_, rule| rule.project_id != project_id);
        state.cycles.retain(|_, cycle| cycle.project_id != project_id);
        state
            .program_increments
            .retain(|_, increment| increment.project_id != project_id);
        state
            .business_cases
            .retain(|_, business_case| business_case.project_id != project_id);
        state
            .activity
            .retain(|_, record| record.project_id != project_id);
        Ok(json!({ "deleted": true, "id": project_id }))
    })();
    json::respond(result, false)
}

async fn archive_project(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let project_id = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Project> = (|| {
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        let mut stored = state
            .projects
            .get_mut(&project_id)
            .ok_or_else(|| UnifiedError::not_found("Project not found"))?;
        stored.archived = true;
        stored.updated_at = now();
        let updated = stored.clone();
        drop(stored);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "project.archived",
            "project",
            Some(project_id.clone()),
            json!({ "name": updated.name }),
        );
        state.emit(
            "project.archived",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        state.notify(
            &updated.owner_id,
            format!("Project archived: {}", updated.name),
            format!("Project {} was archived.", updated.name),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn restore_project(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let project_id = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Project> = (|| {
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        let mut stored = state
            .projects
            .get_mut(&project_id)
            .ok_or_else(|| UnifiedError::not_found("Project not found"))?;
        stored.archived = false;
        stored.updated_at = now();
        let updated = stored.clone();
        drop(stored);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "project.restored",
            "project",
            Some(project_id.clone()),
            json!({ "name": updated.name }),
        );
        state.emit(
            "project.restored",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        state.notify(
            &updated.owner_id,
            format!("Project restored: {}", updated.name),
            format!("Project {} was restored.", updated.name),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn get_project_overview(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let project_id = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<S2ProjectOverview> = (|| {
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        let current = now();
        let tasks: Vec<ProjectTask> = state
            .tasks
            .iter()
            .filter(|entry| entry.value().project_id == project_id)
            .map(|entry| entry.value().clone())
            .collect();
        let tasks_total = tasks.len() as u64;
        let tasks_done = tasks
            .iter()
            .filter(|task| s2_task_is_done(&task.status))
            .count() as u64;
        let tasks_open = tasks_total.saturating_sub(tasks_done);
        let tasks_overdue = tasks
            .iter()
            .filter(|task| {
                !s2_task_is_done(&task.status)
                    && task.status != "cancelled"
                    && task
                        .due_date
                        .is_some_and(|due| due < current)
            })
            .count() as u64;
        let members = state
            .members
            .iter()
            .filter(|entry| entry.value().project_id == project_id)
            .count() as u64;
        let milestones: Vec<ProjectMilestone> = state
            .milestones
            .iter()
            .filter(|entry| entry.value().project_id == project_id)
            .map(|entry| entry.value().clone())
            .collect();
        let milestones_total = milestones.len() as u64;
        let milestones_reached = milestones.iter().filter(|item| item.reached).count() as u64;
        let sprints_active = state
            .sprints
            .iter()
            .filter(|entry| {
                entry.value().project_id == project_id && entry.value().status == "active"
            })
            .count() as u64;
        let mut total_logged_hours = 0.0_f64;
        let mut billable_unbilled_hours = 0.0_f64;
        for entry in state.time_entries.iter() {
            let record = entry.value();
            if record.project_id != project_id {
                continue;
            }
            total_logged_hours += record.hours;
            if record.billable && !record.billed {
                billable_unbilled_hours += record.hours;
            }
        }
        let progress_percent = if tasks_total == 0 {
            0.0
        } else {
            tasks_done as f64 / tasks_total as f64 * 100.0
        };
        Ok(S2ProjectOverview {
            project_id: project_id.clone(),
            tasks_total,
            tasks_open,
            tasks_done,
            tasks_overdue,
            members,
            milestones_total,
            milestones_reached,
            sprints_active,
            total_logged_hours,
            billable_unbilled_hours,
            progress_percent,
        })
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Project members
// ---------------------------------------------------------------------------

async fn list_project_members(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let project_id = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let project = match state.get_project(&project_id) {
        Ok(project) => project,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = require_project_access(
        &state,
        &project,
        viewer_id.as_deref(),
        viewer_role.as_deref(),
    ) {
        return json::error_response(&error, false);
    }
    let sort = params.allowed_sort(&["joined_at"], "joined_at");
    let mut items: Vec<ProjectMember> = state
        .members
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|member| {
            member.project_id == project_id
                && params
                    .filters
                    .get("role")
                    .is_none_or(|role| member.role == *role)
        })
        .collect();
    match sort.as_str() {
        _ => items.sort_by(|a, b| a.joined_at.cmp(&b.joined_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn add_project_member(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<AddProjectMember>,
) -> HttpResponse {
    let project_id = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectMember> = (|| {
        require_capability(&state, viewer_role.as_deref(), "projects:manage_members")?;
        let input = body.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        let user_id = input.user_id.trim().to_string();
        if user_id.is_empty() {
            return Err(UnifiedError::bad_request("user_id must not be empty"));
        }
        let role = match input.role {
            Some(value) if s2_valid_member_role(value.trim()) => value.trim().to_string(),
            Some(value) => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown member role: {}",
                    value.trim()
                )));
            }
            None => "member".to_string(),
        };
        let duplicate = state.members.iter().any(|entry| {
            entry.value().project_id == project_id && entry.value().user_id == user_id
        });
        if duplicate {
            return Err(UnifiedError::bad_request(
                "User is already a member of this project",
            ));
        }
        let member = ProjectMember {
            id: new_id(),
            project_id: project_id.clone(),
            user_id: user_id.clone(),
            role,
            joined_at: now(),
        };
        state.members.insert(member.id.clone(), member.clone());
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "member.added",
            "member",
            Some(member.id.clone()),
            json!({ "user_id": member.user_id, "role": member.role }),
        );
        state.notify(
            &user_id,
            format!("Added to project: {}", project.name),
            format!("You were added to project {} as {}.", project.name, member.role),
        );
        Ok(member)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn update_project_member(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateProjectMember>,
) -> HttpResponse {
    let (project_id, user_id) = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectMember> = (|| {
        require_capability(&state, viewer_role.as_deref(), "projects:manage_members")?;
        let input = body.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        let role = match input.role {
            Some(value) if s2_valid_member_role(value.trim()) => value.trim().to_string(),
            Some(value) => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown member role: {}",
                    value.trim()
                )));
            }
            None => {
                return Err(UnifiedError::bad_request("role must not be empty"));
            }
        };
        // The route carries the member's user id, not the membership
        // record id: resolve the record by (project_id, user_id).
        let current = state
            .members
            .iter()
            .map(|entry| entry.value().clone())
            .find(|member| member.project_id == project_id && member.user_id == user_id)
            .ok_or_else(|| UnifiedError::not_found("Project member not found"))?;
        // Demoting the last owner would leave the project ownerless.
        if current.role == "owner" && role != "owner" {
            let owners = state
                .members
                .iter()
                .filter(|entry| {
                    entry.value().project_id == project_id && entry.value().role == "owner"
                })
                .count();
            if owners <= 1 {
                return Err(UnifiedError::bad_request(
                    "Cannot demote the last owner of the project",
                ));
            }
        }
        let member_id = current.id.clone();
        let mut stored = state
            .members
            .get_mut(&member_id)
            .ok_or_else(|| UnifiedError::not_found("Project member not found"))?;
        stored.role = role;
        let updated = stored.clone();
        drop(stored);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "member.updated",
            "member",
            Some(member_id.clone()),
            json!({ "user_id": updated.user_id, "role": updated.role }),
        );
        state.notify(
            &updated.user_id,
            format!("Role changed in project: {}", project.name),
            format!("Your role in project {} is now {}.", project.name, updated.role),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn remove_project_member(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let (project_id, user_id) = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Value> = (|| {
        require_capability(&state, viewer_role.as_deref(), "projects:manage_members")?;
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        // The route carries the member's user id, not the membership
        // record id: resolve the record by (project_id, user_id).
        let member = state
            .members
            .iter()
            .map(|entry| entry.value().clone())
            .find(|member| member.project_id == project_id && member.user_id == user_id)
            .ok_or_else(|| UnifiedError::not_found("Project member not found"))?;
        // Removing the last owner would leave the project ownerless.
        if member.role == "owner" {
            let owners = state
                .members
                .iter()
                .filter(|entry| {
                    entry.value().project_id == project_id && entry.value().role == "owner"
                })
                .count();
            if owners <= 1 {
                return Err(UnifiedError::bad_request(
                    "Cannot remove the last owner of the project",
                ));
            }
        }
        state.members.remove(&member.id);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "member.removed",
            "member",
            Some(member.id.clone()),
            json!({ "user_id": member.user_id }),
        );
        state.notify(
            &member.user_id,
            format!("Removed from project: {}", project.name),
            format!("You were removed from project {}.", project.name),
        );
        Ok(json!({ "deleted": true, "id": member.id }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Methodologies
// ---------------------------------------------------------------------------

async fn list_methodologies(state: web::Data<Arc<ProjectsState>>) -> HttpResponse {
    let _state = state;
    json::success_response(&METHODOLOGIES, 200)
}

// ---------------------------------------------------------------------------
// Project templates
// ---------------------------------------------------------------------------

async fn list_project_templates(
    state: web::Data<Arc<ProjectsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let sort = params.allowed_sort(&["created_at", "updated_at", "name"], "created_at");
    let mut items: Vec<ProjectTemplate> = state
        .templates
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|template| {
            matches_search(
                &format!("{} {}", template.name, template.description),
                &params,
            ) && params
                .filters
                .get("methodology")
                .is_none_or(|value| template.methodology == *value)
                && params.filters.get("is_system").is_none_or(|value| {
                    template.is_system == (value == "true")
                })
        })
        .collect();
    match sort.as_str() {
        "updated_at" => items.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_project_template(
    state: web::Data<Arc<ProjectsState>>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateProjectTemplate>,
) -> HttpResponse {
    let params = query.into_inner();
    let (_viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectTemplate> = (|| {
        require_capability(&state, viewer_role.as_deref(), "projects:manage")?;
        let input = body.into_inner();
        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Err(UnifiedError::bad_request(
                "Template name must not be empty",
            ));
        }
        let methodology = match input.methodology {
            Some(value) if valid_methodology(value.trim()) => value.trim().to_string(),
            Some(value) => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown methodology: {}",
                    value.trim()
                )));
            }
            None => {
                return Err(UnifiedError::bad_request(
                    "methodology must not be empty",
                ));
            }
        };
        let timestamp = now();
        let template = ProjectTemplate {
            id: new_id(),
            name,
            description: s2_clean_optional(input.description).unwrap_or_default(),
            methodology,
            boards: input.boards.unwrap_or_else(|| json!([])),
            columns: input.columns.unwrap_or_else(|| json!([])),
            statuses: input.statuses.unwrap_or_else(|| json!([])),
            phases: input.phases.unwrap_or_else(|| json!([])),
            custom_fields: input.custom_fields.unwrap_or_else(|| json!({})),
            is_system: input.is_system.unwrap_or(false),
            created_at: timestamp,
            updated_at: timestamp,
        };
        state
            .templates
            .insert(template.id.clone(), template.clone());
        Ok(template)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn get_project_template(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTemplate> = (|| {
        state
            .templates
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Project template not found"))
    })();
    json::respond(result, false)
}

async fn update_project_template(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateProjectTemplate>,
) -> HttpResponse {
    let template_id = path.into_inner();
    let params = query.into_inner();
    let (_viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectTemplate> = (|| {
        require_capability(&state, viewer_role.as_deref(), "projects:manage")?;
        let input = body.into_inner();
        if let Some(methodology) = input.methodology.as_ref() {
            if !valid_methodology(methodology.trim()) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown methodology: {}",
                    methodology.trim()
                )));
            }
        }
        let mut stored = state
            .templates
            .get_mut(&template_id)
            .ok_or_else(|| UnifiedError::not_found("Project template not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request(
                    "Template name must not be empty",
                ));
            }
            stored.name = name.trim().to_string();
        }
        if let Some(description) = input.description {
            stored.description = description.trim().to_string();
        }
        if let Some(methodology) = input.methodology {
            stored.methodology = methodology.trim().to_string();
        }
        if let Some(boards) = input.boards {
            stored.boards = boards;
        }
        if let Some(columns) = input.columns {
            stored.columns = columns;
        }
        if let Some(statuses) = input.statuses {
            stored.statuses = statuses;
        }
        if let Some(phases) = input.phases {
            stored.phases = phases;
        }
        if let Some(custom_fields) = input.custom_fields {
            stored.custom_fields = custom_fields;
        }
        if let Some(is_system) = input.is_system {
            stored.is_system = is_system;
        }
        stored.updated_at = now();
        Ok(stored.clone())
    })();
    json::respond(result, false)
}

async fn delete_project_template(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let template_id = path.into_inner();
    let params = query.into_inner();
    let (_viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Value> = (|| {
        require_capability(&state, viewer_role.as_deref(), "projects:manage")?;
        let template = state
            .templates
            .get(&template_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Project template not found"))?;
        if template.is_system {
            return Err(UnifiedError::forbidden(
                "System templates cannot be deleted",
            ));
        }
        state.templates.remove(&template_id);
        Ok(json!({ "deleted": true, "id": template_id }))
    })();
    json::respond(result, false)
}

async fn apply_project_template(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<ApplyTemplate>,
) -> HttpResponse {
    let project_id = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Project> = (|| {
        require_capability(&state, viewer_role.as_deref(), "projects:manage")?;
        let input = body.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        let template = state
            .templates
            .get(input.template_id.trim())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Project template not found"))?;
        let default_kind = methodology_profile(&project.methodology)
            .map(|profile| profile.default_board_kind.to_string())
            .unwrap_or_else(|| "kanban".to_string());
        let timestamp = now();
        // Boards from the template, with embedded columns when the
        // board object carries its own `columns` array.
        let mut created_boards: Vec<(String, String)> = Vec::new();
        let mut embedded_columns = false;
        let board_values = template
            .boards
            .as_array()
            .cloned()
            .unwrap_or_default();
        for board_value in &board_values {
            let Some(board_object) = board_value.as_object() else {
                continue;
            };
            let board_name = board_object
                .get("name")
                .and_then(|entry| entry.as_str())
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .unwrap_or("Board")
                .to_string();
            let kind = board_object
                .get("kind")
                .and_then(|entry| entry.as_str())
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .unwrap_or(&default_kind)
                .to_string();
            let wip_limit = board_object
                .get("wip_limit")
                .and_then(|entry| entry.as_u64())
                .map(|limit| limit as u32)
                .filter(|limit| *limit > 0);
            let board = ProjectBoard {
                id: new_id(),
                project_id: project_id.clone(),
                name: board_name.clone(),
                kind,
                wip_limit,
                created_at: timestamp,
                updated_at: timestamp,
            };
            state.boards.insert(board.id.clone(), board.clone());
            created_boards.push((board.id.clone(), board_name));
            if let Some(columns) = board_object.get("columns").and_then(|entry| entry.as_array())
            {
                for (position, column_value) in columns.iter().enumerate() {
                    if let Some(column) =
                        s2_column_from_value(&board.id, column_value, position as u32)
                    {
                        state.columns.insert(column.id.clone(), column);
                        embedded_columns = true;
                    }
                }
            }
        }
        // Standalone `columns` array: attach each column to the board
        // named by `board_name` / `board_id` when present, otherwise
        // to the first board the template created.
        if !embedded_columns {
            if let Some(columns) = template.columns.as_array() {
                for (position, column_value) in columns.iter().enumerate() {
                    let target_board = column_value
                        .get("board_name")
                        .and_then(|entry| entry.as_str())
                        .and_then(|name| {
                            created_boards
                                .iter()
                                .find(|(_, board_name)| board_name == name)
                                .map(|(board_id, _)| board_id.clone())
                        })
                        .or_else(|| {
                            column_value
                                .get("board_id")
                                .and_then(|entry| entry.as_str())
                                .and_then(|board_id| {
                                    created_boards
                                        .iter()
                                        .find(|(id, _)| id == board_id)
                                        .map(|(id, _)| id.clone())
                                })
                        })
                        .or_else(|| {
                            created_boards
                                .first()
                                .map(|(board_id, _)| board_id.clone())
                        });
                    let Some(board_id) = target_board else {
                        continue;
                    };
                    if let Some(column) =
                        s2_column_from_value(&board_id, column_value, position as u32)
                    {
                        state.columns.insert(column.id.clone(), column);
                    }
                }
            }
        }
        // Phases from the template `phases` array.
        if let Some(phases) = template.phases.as_array() {
            for (index, phase_value) in phases.iter().enumerate() {
                let Some(phase_object) = phase_value.as_object() else {
                    continue;
                };
                let Some(phase_name) = phase_object
                    .get("name")
                    .and_then(|entry| entry.as_str())
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                else {
                    continue;
                };
                let position = phase_object
                    .get("position")
                    .and_then(|entry| entry.as_u64())
                    .or_else(|| phase_object.get("order").and_then(|entry| entry.as_u64()))
                    .unwrap_or(index as u64) as u32;
                let parse_date = |key: &str| {
                    phase_object
                        .get(key)
                        .and_then(|entry| entry.as_str())
                        .and_then(|text| {
                            DateTime::parse_from_rfc3339(text)
                                .ok()
                                .map(|parsed| parsed.with_timezone(&Utc))
                        })
                };
                let phase = ProjectPhase {
                    id: new_id(),
                    project_id: project_id.clone(),
                    name: phase_name.to_string(),
                    kind: phase_object
                        .get("kind")
                        .and_then(|entry| entry.as_str())
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .unwrap_or("waterfall")
                        .to_string(),
                    position,
                    status: "planned".to_string(),
                    gate_status: "pending".to_string(),
                    gate_decision: None,
                    start_date: parse_date("start_date"),
                    end_date: parse_date("due_date").or_else(|| parse_date("end_date")),
                    created_at: timestamp,
                    updated_at: timestamp,
                };
                state.phases.insert(phase.id.clone(), phase);
            }
        }
        // The template's custom fields become the project's own.
        let mut stored = state
            .projects
            .get_mut(&project_id)
            .ok_or_else(|| UnifiedError::not_found("Project not found"))?;
        stored.custom_field_definitions = template.custom_fields.clone();
        stored.updated_at = now();
        let updated = stored.clone();
        drop(stored);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "template.applied",
            "template",
            Some(template.id.clone()),
            json!({ "template_id": template.id, "template": template.name }),
        );
        state.emit(
            "project.template.applied",
            json!({
                "project_id": project_id,
                "template_id": template.id,
                "project": serde_json::to_value(&updated).unwrap_or(Value::Null),
            }),
        );
        state.notify(
            &updated.owner_id,
            format!("Template applied to project: {}", updated.name),
            format!("Template {} was applied to project {}.", template.name, updated.name),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Project boards
// ---------------------------------------------------------------------------

async fn list_project_boards(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let project_id = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let project = match state.get_project(&project_id) {
        Ok(project) => project,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = require_project_access(
        &state,
        &project,
        viewer_id.as_deref(),
        viewer_role.as_deref(),
    ) {
        return json::error_response(&error, false);
    }
    let sort = params.allowed_sort(&["created_at", "updated_at", "name"], "created_at");
    let mut items: Vec<ProjectBoard> = state
        .boards
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|board| {
            board.project_id == project_id
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|kind| board.kind == *kind)
        })
        .collect();
    match sort.as_str() {
        "updated_at" => items.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_project_board(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateProjectBoard>,
) -> HttpResponse {
    let project_id = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectBoard> = (|| {
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        let input = body.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Err(UnifiedError::bad_request("Board name must not be empty"));
        }
        if let Some(limit) = input.wip_limit {
            if limit == 0 {
                return Err(UnifiedError::bad_request(
                    "wip_limit must be greater than 0",
                ));
            }
        }
        let kind = match s2_clean_optional(input.kind) {
            Some(kind) => kind,
            None => methodology_profile(&project.methodology)
                .map(|profile| profile.default_board_kind.to_string())
                .unwrap_or_else(|| "kanban".to_string()),
        };
        let timestamp = now();
        let board = ProjectBoard {
            id: new_id(),
            project_id: project_id.clone(),
            name,
            kind,
            wip_limit: input.wip_limit,
            created_at: timestamp,
            updated_at: timestamp,
        };
        state.boards.insert(board.id.clone(), board.clone());
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "board.created",
            "board",
            Some(board.id.clone()),
            json!({ "name": board.name, "kind": board.kind }),
        );
        Ok(board)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn get_project_board(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let (project_id, board_id) = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectBoard> = (|| {
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        s2_get_board(&state, &project_id, &board_id)
    })();
    json::respond(result, false)
}

async fn update_project_board(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateProjectBoard>,
) -> HttpResponse {
    let (project_id, board_id) = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectBoard> = (|| {
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        let input = body.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        s2_get_board(&state, &project_id, &board_id)?;
        if let Some(limit) = input.wip_limit {
            if limit == 0 {
                return Err(UnifiedError::bad_request(
                    "wip_limit must be greater than 0",
                ));
            }
        }
        let mut stored = state
            .boards
            .get_mut(&board_id)
            .ok_or_else(|| UnifiedError::not_found("Project board not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Board name must not be empty"));
            }
            stored.name = name.trim().to_string();
        }
        if let Some(kind) = input.kind {
            if kind.trim().is_empty() {
                return Err(UnifiedError::bad_request("Board kind must not be empty"));
            }
            stored.kind = kind.trim().to_string();
        }
        if input.wip_limit.is_some() {
            stored.wip_limit = input.wip_limit;
        }
        stored.updated_at = now();
        let updated = stored.clone();
        drop(stored);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "board.updated",
            "board",
            Some(board_id.clone()),
            json!({ "name": updated.name }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_project_board(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let (project_id, board_id) = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Value> = (|| {
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        let board = s2_get_board(&state, &project_id, &board_id)?;
        state.boards.remove(&board_id);
        state.columns.retain(|_, column| column.board_id != board_id);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "board.deleted",
            "board",
            Some(board_id.clone()),
            json!({ "name": board.name }),
        );
        Ok(json!({ "deleted": true, "id": board_id }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Board columns
// ---------------------------------------------------------------------------

async fn list_board_columns(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let (project_id, board_id) = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let project = match state.get_project(&project_id) {
        Ok(project) => project,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = require_project_access(
        &state,
        &project,
        viewer_id.as_deref(),
        viewer_role.as_deref(),
    ) {
        return json::error_response(&error, false);
    }
    if let Err(error) = s2_get_board(&state, &project_id, &board_id) {
        return json::error_response(&error, false);
    }
    let sort = params.allowed_sort(&["position", "name"], "position");
    let mut items: Vec<ProjectBoardColumn> = state
        .columns
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|column| column.board_id == board_id)
        .collect();
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        _ => items.sort_by(|a, b| a.position.cmp(&b.position)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_board_column(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateBoardColumn>,
) -> HttpResponse {
    let (project_id, board_id) = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectBoardColumn> = (|| {
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        let input = body.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        s2_get_board(&state, &project_id, &board_id)?;
        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Err(UnifiedError::bad_request(
                "Column name must not be empty",
            ));
        }
        if let Some(limit) = input.wip_limit {
            if limit == 0 {
                return Err(UnifiedError::bad_request(
                    "wip_limit must be greater than 0",
                ));
            }
        }
        let position = match input.position {
            Some(position) => position,
            None => state
                .columns
                .iter()
                .filter(|entry| entry.value().board_id == board_id)
                .map(|entry| entry.value().position)
                .max()
                .map(|max| max.saturating_add(1))
                .unwrap_or(0),
        };
        let status_key = match input.status_key {
            Some(key) if !key.trim().is_empty() => s2_normalize_status_key(&key),
            _ => s2_normalize_status_key(&name),
        };
        let column = ProjectBoardColumn {
            id: new_id(),
            board_id: board_id.clone(),
            name,
            position,
            status_key,
            wip_limit: input.wip_limit,
        };
        state.columns.insert(column.id.clone(), column.clone());
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "column.created",
            "column",
            Some(column.id.clone()),
            json!({ "name": column.name, "board_id": board_id }),
        );
        Ok(column)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn update_board_column(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateBoardColumn>,
) -> HttpResponse {
    let (project_id, board_id, column_id) = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectBoardColumn> = (|| {
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        let input = body.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        s2_get_board(&state, &project_id, &board_id)?;
        s2_get_column(&state, &board_id, &column_id)?;
        if let Some(limit) = input.wip_limit {
            if limit == 0 {
                return Err(UnifiedError::bad_request(
                    "wip_limit must be greater than 0",
                ));
            }
        }
        let mut stored = state
            .columns
            .get_mut(&column_id)
            .ok_or_else(|| UnifiedError::not_found("Board column not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request(
                    "Column name must not be empty",
                ));
            }
            stored.name = name.trim().to_string();
        }
        if let Some(status_key) = input.status_key {
            if status_key.trim().is_empty() {
                return Err(UnifiedError::bad_request(
                    "status_key must not be empty",
                ));
            }
            stored.status_key = s2_normalize_status_key(&status_key);
        }
        if let Some(position) = input.position {
            stored.position = position;
        }
        if input.wip_limit.is_some() {
            stored.wip_limit = input.wip_limit;
        }
        let updated = stored.clone();
        drop(stored);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "column.updated",
            "column",
            Some(column_id.clone()),
            json!({ "name": updated.name, "board_id": board_id }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_board_column(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let (project_id, board_id, column_id) = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Value> = (|| {
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        s2_get_board(&state, &project_id, &board_id)?;
        let column = s2_get_column(&state, &board_id, &column_id)?;
        state.columns.remove(&column_id);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "column.deleted",
            "column",
            Some(column_id.clone()),
            json!({ "name": column.name, "board_id": board_id }),
        );
        Ok(json!({ "deleted": true, "id": column_id }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Project milestones
// ---------------------------------------------------------------------------

async fn list_project_milestones(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let project_id = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let project = match state.get_project(&project_id) {
        Ok(project) => project,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = require_project_access(
        &state,
        &project,
        viewer_id.as_deref(),
        viewer_role.as_deref(),
    ) {
        return json::error_response(&error, false);
    }
    let sort = params.allowed_sort(&["created_at", "due_date", "name"], "created_at");
    let mut items: Vec<ProjectMilestone> = state
        .milestones
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|milestone| {
            milestone.project_id == project_id
                && params.filters.get("reached").is_none_or(|value| {
                    milestone.reached == (value == "true")
                })
        })
        .collect();
    match sort.as_str() {
        "due_date" => items.sort_by(|a, b| a.due_date.cmp(&b.due_date)),
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_project_milestone(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateProjectMilestone>,
) -> HttpResponse {
    let project_id = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectMilestone> = (|| {
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        let input = body.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Err(UnifiedError::bad_request(
                "Milestone name must not be empty",
            ));
        }
        let timestamp = now();
        let milestone = ProjectMilestone {
            id: new_id(),
            project_id: project_id.clone(),
            name,
            description: s2_clean_optional(input.description).unwrap_or_default(),
            due_date: input.due_date,
            reached: false,
            reached_at: None,
            created_at: timestamp,
            updated_at: timestamp,
        };
        state
            .milestones
            .insert(milestone.id.clone(), milestone.clone());
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "milestone.created",
            "milestone",
            Some(milestone.id.clone()),
            json!({ "name": milestone.name }),
        );
        Ok(milestone)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn get_project_milestone(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let (project_id, milestone_id) = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectMilestone> = (|| {
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        s2_get_milestone(&state, &project_id, &milestone_id)
    })();
    json::respond(result, false)
}

async fn update_project_milestone(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateProjectMilestone>,
) -> HttpResponse {
    let (project_id, milestone_id) = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectMilestone> = (|| {
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        let input = body.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        s2_get_milestone(&state, &project_id, &milestone_id)?;
        let mut stored = state
            .milestones
            .get_mut(&milestone_id)
            .ok_or_else(|| UnifiedError::not_found("Project milestone not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request(
                    "Milestone name must not be empty",
                ));
            }
            stored.name = name.trim().to_string();
        }
        if let Some(description) = input.description {
            stored.description = description.trim().to_string();
        }
        if input.due_date.is_some() {
            stored.due_date = input.due_date;
        }
        if let Some(reached) = input.reached {
            stored.reached = reached;
            stored.reached_at = if reached {
                stored.reached_at.or_else(|| Some(now()))
            } else {
                None
            };
        }
        stored.updated_at = now();
        let updated = stored.clone();
        drop(stored);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "milestone.updated",
            "milestone",
            Some(milestone_id.clone()),
            json!({ "name": updated.name }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_project_milestone(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let (project_id, milestone_id) = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Value> = (|| {
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        let milestone = s2_get_milestone(&state, &project_id, &milestone_id)?;
        state.milestones.remove(&milestone_id);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "milestone.deleted",
            "milestone",
            Some(milestone_id.clone()),
            json!({ "name": milestone.name }),
        );
        Ok(json!({ "deleted": true, "id": milestone_id }))
    })();
    json::respond(result, false)
}

async fn reach_project_milestone(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let (project_id, milestone_id) = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectMilestone> = (|| {
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        s2_get_milestone(&state, &project_id, &milestone_id)?;
        let mut stored = state
            .milestones
            .get_mut(&milestone_id)
            .ok_or_else(|| UnifiedError::not_found("Project milestone not found"))?;
        stored.reached = true;
        stored.reached_at = Some(now());
        stored.updated_at = now();
        let updated = stored.clone();
        drop(stored);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "milestone.reached",
            "milestone",
            Some(milestone_id.clone()),
            json!({ "name": updated.name }),
        );
        state.emit(
            "project.milestone.reached",
            json!({
                "project_id": project_id,
                "milestone_id": milestone_id,
                "milestone": serde_json::to_value(&updated).unwrap_or(Value::Null),
            }),
        );
        state.notify(
            &project.owner_id,
            format!("Milestone reached: {}", updated.name),
            format!("Milestone {} was reached in project {}.", updated.name, project.name),
        );
        let member_ids: Vec<String> = state
            .members
            .iter()
            .filter(|entry| {
                entry.value().project_id == project_id
                    && entry.value().user_id != project.owner_id
            })
            .map(|entry| entry.value().user_id.clone())
            .collect();
        for user_id in member_ids {
            state.notify(
                &user_id,
                format!("Milestone reached: {}", updated.name),
                format!(
                    "Milestone {} was reached in project {}.",
                    updated.name, project.name
                ),
            );
        }
        Ok(updated)
    })();
    json::respond(result, false)
}
// Project tasks, subtasks, dependencies, comments, watchers, attachments,
// automation rules, and the ticket bridge for the Projects module.
//
// Section file: this file is assembled into src/modules/projects.rs after
// the section that owns the imports, the entity structs, the ProjectsState
// helpers (now, new_id, paginate, matches_search, viewer_of,
// require_capability, require_project_access, methodology_profile) and the
// module definition. Nothing defined there is redefined here. Every private
// helper in this file is prefixed with `s3_` so it cannot collide with
// helpers from other sections.
//
// Path shapes assumed by these handlers (routes live in the assembling
// section): project-scoped task lists and creation take the project id;
// single-task handlers take the task id; nested comment, watcher,
// attachment and dependency removals take (task id, child id); automation
// rule detail handlers take (project id, rule id). The ticket bridge
// creation handler takes its project id from the request body.
//
// Viewer identity for reads comes from the pagination query through
// viewer_of; for writes it comes from the `viewer_id` / `viewer_role`
// query parameters (S3ViewerQuery), matching the viewer convention used
// by the tickets module.

// ---------------------------------------------------------------------------
// Input structs
// ---------------------------------------------------------------------------

/// Query parameters carrying the caller's identity for write handlers.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct S3ViewerQuery {
    #[serde(default)]
    pub viewer_id: Option<String>,
    #[serde(default)]
    pub viewer_role: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProjectTask {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub task_type: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub assignee_id: Option<String>,
    #[serde(default)]
    pub reporter_id: Option<String>,
    #[serde(default)]
    pub estimate_points: Option<f64>,
    #[serde(default)]
    pub estimate_hours: Option<f64>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub custom_values: Value,
    #[serde(default)]
    pub start_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub due_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub sprint_id: Option<String>,
    #[serde(default)]
    pub phase_id: Option<String>,
    #[serde(default)]
    pub cycle_id: Option<String>,
    #[serde(default)]
    pub pi_id: Option<String>,
    #[serde(default)]
    pub milestone_id: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub epic_id: Option<String>,
    #[serde(default)]
    pub board_id: Option<String>,
    #[serde(default)]
    pub process_group: Option<String>,
    #[serde(default)]
    pub knowledge_area: Option<String>,
    #[serde(default)]
    pub waste_flag: Option<bool>,
    #[serde(default)]
    pub pair_partner_id: Option<String>,
    #[serde(default)]
    pub test_first: Option<bool>,
    #[serde(default)]
    pub dmaic_phase: Option<String>,
    #[serde(default)]
    pub appetite_weeks: Option<u32>,
}

/// Partial task update. Nullable reference and date fields use a double
/// Option: absent means unchanged, explicit null clears the value.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateProjectTask {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub assignee_id: Option<Option<String>>,
    #[serde(default)]
    pub estimate_points: Option<f64>,
    #[serde(default)]
    pub estimate_hours: Option<f64>,
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    #[serde(default)]
    pub custom_values: Option<Value>,
    #[serde(default)]
    pub start_date: Option<Option<DateTime<Utc>>>,
    #[serde(default)]
    pub due_date: Option<Option<DateTime<Utc>>>,
    #[serde(default)]
    pub sprint_id: Option<Option<String>>,
    #[serde(default)]
    pub phase_id: Option<Option<String>>,
    #[serde(default)]
    pub cycle_id: Option<Option<String>>,
    #[serde(default)]
    pub pi_id: Option<Option<String>>,
    #[serde(default)]
    pub milestone_id: Option<Option<String>>,
    #[serde(default)]
    pub process_group: Option<Option<String>>,
    #[serde(default)]
    pub knowledge_area: Option<Option<String>>,
    #[serde(default)]
    pub waste_flag: Option<bool>,
    #[serde(default)]
    pub pair_partner_id: Option<Option<String>>,
    #[serde(default)]
    pub test_first: Option<bool>,
    #[serde(default)]
    pub dmaic_phase: Option<Option<String>>,
    #[serde(default)]
    pub appetite_weeks: Option<Option<u32>>,
    #[serde(default)]
    pub board_id: Option<Option<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MoveTask {
    #[serde(default)]
    pub board_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub column_status_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssignTask {
    pub assignee_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddTaskDependency {
    pub depends_on_task_id: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub lag_days: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddTaskComment {
    pub author_id: String,
    pub body: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTaskComment {
    pub body: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddTaskWatcher {
    pub user_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddTaskAttachment {
    pub target: String,
    pub key: String,
    #[serde(default)]
    pub stored_key: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    pub uploaded_by: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateAutomationRule {
    pub name: String,
    pub trigger: String,
    #[serde(default)]
    pub conditions: Value,
    pub actions: Value,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateAutomationRule {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub trigger: Option<String>,
    #[serde(default)]
    pub conditions: Option<Value>,
    #[serde(default)]
    pub actions: Option<Value>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ConvertTaskToTicket {
    #[serde(default)]
    pub department_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TaskFromTicket {
    pub ticket_id: String,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    pub user_id: String,
    pub project_id: String,
    #[serde(default)]
    pub department_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Private helpers (s3_ prefixed; nothing here may collide with s1)
// ---------------------------------------------------------------------------

fn s3_valid_task_type(task_type: &str) -> bool {
    matches!(
        task_type,
        "task" | "story" | "bug" | "spike" | "feature" | "enabler" | "chore"
    )
}

fn s3_valid_priority(priority: &str) -> bool {
    matches!(priority, "low" | "normal" | "high" | "urgent")
}

fn s3_priority_rank(priority: &str) -> u8 {
    match priority {
        "low" => 0,
        "high" => 2,
        "urgent" => 3,
        _ => 1,
    }
}

fn s3_valid_dependency_kind(kind: &str) -> bool {
    matches!(
        kind,
        "finish_start" | "start_start" | "finish_finish" | "start_finish" | "blocks" | "related"
    )
}

fn s3_valid_rule_trigger(trigger: &str) -> bool {
    matches!(
        trigger,
        "task_created" | "status_changed" | "task_completed" | "due_soon" | "label_added"
    )
}

/// A status is usable when it is one of the methodology profile defaults;
/// `done` and `cancelled` are always allowed. An unknown methodology
/// fails open so future methodologies keep working.
fn s3_methodology_allows_status(project: &Project, status: &str) -> bool {
    if status == "done" || status == "cancelled" {
        return true;
    }
    match methodology_profile(&project.methodology) {
        Some(profile) => profile
            .default_statuses
            .iter()
            .any(|candidate| candidate.to_string() == status),
        None => true,
    }
}

/// The methodology's first default status, used for new tasks.
fn s3_default_status(project: &Project) -> String {
    methodology_profile(&project.methodology)
        .and_then(|profile| {
            profile
                .default_statuses
                .first()
                .map(|status| status.to_string())
        })
        .unwrap_or_else(|| "todo".to_string())
}

/// The methodology's first non-terminal status, used when reopening.
fn s3_first_open_status(project: &Project) -> String {
    methodology_profile(&project.methodology)
        .and_then(|profile| {
            profile
                .default_statuses
                .iter()
                .map(|status| status.to_string())
                .find(|status| status != "done" && status != "cancelled")
        })
        .unwrap_or_else(|| "todo".to_string())
}

/// Sort a task list by an allowed sort field, then apply the direction.
fn s3_sort_tasks(items: &mut Vec<ProjectTask>, sort: &str, descending: bool) {
    match sort {
        "updated_at" => items.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
        "due_date" => items.sort_by(|a, b| {
            (a.due_date.is_none(), a.due_date).cmp(&(b.due_date.is_none(), b.due_date))
        }),
        "priority" => items.sort_by_key(|task| s3_priority_rank(&task.priority)),
        "estimate_points" => items.sort_by(|a, b| {
            a.estimate_points
                .unwrap_or(0.0)
                .partial_cmp(&b.estimate_points.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if descending {
        items.reverse();
    }
}

// --- Container references: every referenced container must exist in the
// same project, otherwise the reference is rejected as a bad request. ---

fn s3_require_board_in_project(
    state: &ProjectsState,
    project_id: &str,
    board_id: &str,
) -> UnifiedResult<()> {
    match state.boards.get(board_id) {
        Some(board) if board.value().project_id == project_id => Ok(()),
        _ => Err(UnifiedError::bad_request(
            "board_id does not reference a board in this project",
        )),
    }
}

fn s3_require_sprint_in_project(
    state: &ProjectsState,
    project_id: &str,
    sprint_id: &str,
) -> UnifiedResult<()> {
    match state.sprints.get(sprint_id) {
        Some(sprint) if sprint.value().project_id == project_id => Ok(()),
        _ => Err(UnifiedError::bad_request(
            "sprint_id does not reference a sprint in this project",
        )),
    }
}

fn s3_require_phase_in_project(
    state: &ProjectsState,
    project_id: &str,
    phase_id: &str,
) -> UnifiedResult<()> {
    match state.phases.get(phase_id) {
        Some(phase) if phase.value().project_id == project_id => Ok(()),
        _ => Err(UnifiedError::bad_request(
            "phase_id does not reference a phase in this project",
        )),
    }
}

fn s3_require_cycle_in_project(
    state: &ProjectsState,
    project_id: &str,
    cycle_id: &str,
) -> UnifiedResult<()> {
    match state.cycles.get(cycle_id) {
        Some(cycle) if cycle.value().project_id == project_id => Ok(()),
        _ => Err(UnifiedError::bad_request(
            "cycle_id does not reference a cycle in this project",
        )),
    }
}

fn s3_require_pi_in_project(
    state: &ProjectsState,
    project_id: &str,
    pi_id: &str,
) -> UnifiedResult<()> {
    match state.program_increments.get(pi_id) {
        Some(increment) if increment.value().project_id == project_id => Ok(()),
        _ => Err(UnifiedError::bad_request(
            "pi_id does not reference a program increment in this project",
        )),
    }
}

fn s3_require_milestone_in_project(
    state: &ProjectsState,
    project_id: &str,
    milestone_id: &str,
) -> UnifiedResult<()> {
    match state.milestones.get(milestone_id) {
        Some(milestone) if milestone.value().project_id == project_id => Ok(()),
        _ => Err(UnifiedError::bad_request(
            "milestone_id does not reference a milestone in this project",
        )),
    }
}

fn s3_validate_task_refs(
    state: &ProjectsState,
    project_id: &str,
    board_id: Option<&str>,
    sprint_id: Option<&str>,
    phase_id: Option<&str>,
    cycle_id: Option<&str>,
    pi_id: Option<&str>,
    milestone_id: Option<&str>,
) -> UnifiedResult<()> {
    if let Some(board_id) = board_id {
        s3_require_board_in_project(state, project_id, board_id)?;
    }
    if let Some(sprint_id) = sprint_id {
        s3_require_sprint_in_project(state, project_id, sprint_id)?;
    }
    if let Some(phase_id) = phase_id {
        s3_require_phase_in_project(state, project_id, phase_id)?;
    }
    if let Some(cycle_id) = cycle_id {
        s3_require_cycle_in_project(state, project_id, cycle_id)?;
    }
    if let Some(pi_id) = pi_id {
        s3_require_pi_in_project(state, project_id, pi_id)?;
    }
    if let Some(milestone_id) = milestone_id {
        s3_require_milestone_in_project(state, project_id, milestone_id)?;
    }
    Ok(())
}

// --- Board columns and work-in-progress limits. ---

/// Find the column for a status key, scoped to the project and, when a
/// board is known, to that board. Columns carry only a `board_id`, so
/// project scope is resolved through the column's board.
fn s3_find_column(
    state: &ProjectsState,
    project_id: &str,
    board_id: Option<&str>,
    status_key: &str,
) -> Option<ProjectBoardColumn> {
    state
        .columns
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|column| {
            column.status_key == status_key
                && board_id.is_none_or(|board_id| column.board_id == board_id)
        })
        .find(|column| {
            state
                .boards
                .get(&column.board_id)
                .is_some_and(|board| board.value().project_id == project_id)
        })
}

/// Reject the move when the target column has a WIP limit and is already
/// full. The moving task itself is not counted when it already sits in
/// that column. Columns without a limit never block a move.
fn s3_check_wip_limit(
    state: &ProjectsState,
    project_id: &str,
    board_id: Option<&str>,
    status: &str,
    moving_task_id: &str,
) -> UnifiedResult<()> {
    let Some(column) = s3_find_column(state, project_id, board_id, status) else {
        return Ok(());
    };
    let Some(limit) = column.wip_limit else {
        return Ok(());
    };
    let occupying = state
        .tasks
        .iter()
        .filter(|entry| {
            let task = entry.value();
            task.project_id == project_id
                && task.board_id.as_deref() == board_id
                && task.status == status
                && task.id != moving_task_id
        })
        .count();
    if occupying as u32 >= limit {
        return Err(UnifiedError::bad_request(format!(
            "Target column \"{status}\" has reached its work-in-progress limit of {limit}"
        )));
    }
    Ok(())
}

// --- Task tree and cascade delete. ---

/// Collect a task id plus every descendant subtask id (breadth first).
fn s3_collect_task_tree(state: &ProjectsState, root_id: &str) -> Vec<String> {
    let mut collected = vec![root_id.to_string()];
    let mut index = 0;
    while index < collected.len() {
        let parent_id = collected[index].clone();
        index += 1;
        for entry in state.tasks.iter() {
            let task = entry.value();
            if task.parent_id.as_deref() == Some(parent_id.as_str())
                && !collected.contains(&task.id)
            {
                collected.push(task.id.clone());
            }
        }
    }
    collected
}

/// Delete a task and its whole subtask tree, plus every dependency (in
/// either direction), comment, watcher, attachment and time entry that
/// belongs to any of them. Returns the number of tasks removed.
fn s3_delete_task_cascade(state: &ProjectsState, task_id: &str) -> usize {
    let doomed = s3_collect_task_tree(state, task_id);
    let doomed_set: std::collections::HashSet<&str> =
        doomed.iter().map(String::as_str).collect();
    state.dependencies.retain(|_, dependency| {
        !doomed_set.contains(dependency.task_id.as_str())
            && !doomed_set.contains(dependency.depends_on_task_id.as_str())
    });
    state
        .comments
        .retain(|_, comment| !doomed_set.contains(comment.task_id.as_str()));
    state
        .watchers
        .retain(|_, watcher| !doomed_set.contains(watcher.task_id.as_str()));
    state
        .attachments
        .retain(|_, attachment| !doomed_set.contains(attachment.task_id.as_str()));
    state
        .time_entries
        .retain(|_, entry| !doomed_set.contains(entry.task_id.as_str()));
    let count = doomed.len();
    for id in doomed {
        state.tasks.remove(id.as_str());
    }
    count
}

// --- Automation rules. ---

/// Validate the actions array of a rule: non-empty, every action known,
/// and every action carrying the parameters it needs. `set_status`
/// values are checked against the project methodology.
fn s3_validate_rule_actions(project: &Project, actions: &Value) -> UnifiedResult<()> {
    let Some(list) = actions.as_array() else {
        return Err(UnifiedError::bad_request(
            "actions must be a JSON array",
        ));
    };
    if list.is_empty() {
        return Err(UnifiedError::bad_request(
            "actions must be a non-empty JSON array",
        ));
    }
    for action in list {
        let Some(kind) = action.get("action").and_then(|value| value.as_str()) else {
            return Err(UnifiedError::bad_request(
                "Each action must have an action name",
            ));
        };
        match kind {
            "set_status" => {
                let Some(status) = action.get("status").and_then(|value| value.as_str())
                else {
                    return Err(UnifiedError::bad_request(
                        "set_status action requires a status",
                    ));
                };
                if !s3_methodology_allows_status(project, status) {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown status for this project methodology: {status}"
                    )));
                }
            }
            "assign" => {
                let has_target = action
                    .get("assignee_id")
                    .and_then(|value| value.as_str())
                    .is_some()
                    || action
                        .get("user_id")
                        .and_then(|value| value.as_str())
                        .is_some();
                if !has_target {
                    return Err(UnifiedError::bad_request(
                        "assign action requires assignee_id",
                    ));
                }
            }
            "add_label" => match action.get("label").and_then(|value| value.as_str()) {
                Some(label) if !label.trim().is_empty() => {}
                _ => {
                    return Err(UnifiedError::bad_request(
                        "add_label action requires a label",
                    ));
                }
            },
            "notify" => {}
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown automation action: {other}"
                )));
            }
        }
    }
    Ok(())
}

/// Conditions JSON supports {status?, assignee_id?, label?, overdue?}.
/// Every present condition must hold for the rule to match.
fn s3_rule_conditions_match(task: &ProjectTask, conditions: &Value) -> bool {
    if let Some(status) = conditions.get("status").and_then(|value| value.as_str()) {
        if task.status != status {
            return false;
        }
    }
    if let Some(assignee_id) = conditions
        .get("assignee_id")
        .and_then(|value| value.as_str())
    {
        if task.assignee_id.as_deref() != Some(assignee_id) {
            return false;
        }
    }
    if let Some(label) = conditions.get("label").and_then(|value| value.as_str()) {
        if !task.labels.iter().any(|task_label| task_label == label) {
            return false;
        }
    }
    if let Some(overdue) = conditions.get("overdue").and_then(|value| value.as_bool()) {
        let is_overdue = match task.due_date {
            Some(due_date) => {
                due_date < now() && task.status != "done" && task.status != "cancelled"
            }
            None => false,
        };
        if is_overdue != overdue {
            return false;
        }
    }
    true
}

/// Apply one rule's actions to a task in memory. Returns the number of
/// actions that actually changed something (or sent a notification).
/// Invalid action parameters are skipped, never fatal: a rule must not
/// break the task write that triggered it.
fn s3_apply_rule_actions(
    state: &ProjectsState,
    project: &Project,
    task: &mut ProjectTask,
    rule: &ProjectAutomationRule,
) -> usize {
    let Some(actions) = rule.actions.as_array() else {
        return 0;
    };
    let mut applied = 0;
    for action in actions {
        let Some(kind) = action.get("action").and_then(|value| value.as_str()) else {
            continue;
        };
        match kind {
            "set_status" => {
                if let Some(status) = action.get("status").and_then(|value| value.as_str()) {
                    if s3_methodology_allows_status(project, status) && task.status != status {
                        if status == "done" {
                            task.completed_at = Some(now());
                        } else if task.status == "done" {
                            task.completed_at = None;
                        }
                        task.status = status.to_string();
                        applied += 1;
                    }
                }
            }
            "assign" => {
                let target = action
                    .get("assignee_id")
                    .and_then(|value| value.as_str())
                    .or_else(|| action.get("user_id").and_then(|value| value.as_str()));
                if let Some(user_id) = target {
                    if state.is_member(&project.id, user_id)
                        && task.assignee_id.as_deref() != Some(user_id)
                    {
                        task.assignee_id = Some(user_id.to_string());
                        applied += 1;
                    }
                }
            }
            "add_label" => {
                if let Some(label) = action.get("label").and_then(|value| value.as_str()) {
                    if !label.trim().is_empty()
                        && !task.labels.iter().any(|task_label| task_label == label)
                    {
                        task.labels.push(label.to_string());
                        applied += 1;
                    }
                }
            }
            "notify" => {
                let target = action
                    .get("user_id")
                    .and_then(|value| value.as_str())
                    .or_else(|| action.get("assignee_id").and_then(|value| value.as_str()))
                    .map(str::to_string)
                    .or_else(|| task.assignee_id.clone())
                    .unwrap_or_else(|| task.reporter_id.clone());
                let title = action
                    .get("title")
                    .and_then(|value| value.as_str())
                    .unwrap_or("Project automation")
                    .to_string();
                let body = action
                    .get("body")
                    .and_then(|value| value.as_str())
                    .unwrap_or("An automation rule ran for this task.")
                    .to_string();
                state.notify(&target, title, body);
                applied += 1;
            }
            _ => {}
        }
    }
    applied
}

/// Run every enabled rule of the project whose trigger matches, against
/// the current stored copy of the task. One pass only: actions never
/// chain into further rule runs, so rules cannot loop. Rules that apply
/// at least one action bump their own run count and last run time.
fn apply_automation_rules(
    state: &ProjectsState,
    project: &Project,
    task: &ProjectTask,
    trigger: &str,
) {
    let mut rules: Vec<ProjectAutomationRule> = state
        .automation_rules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rule| rule.project_id == project.id && rule.enabled && rule.trigger == trigger)
        .collect();
    rules.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    for rule in rules {
        let Some(mut current) = state
            .tasks
            .get(task.id.as_str())
            .map(|entry| entry.value().clone())
        else {
            continue;
        };
        if !s3_rule_conditions_match(&current, &rule.conditions) {
            continue;
        }
        let applied = s3_apply_rule_actions(state, project, &mut current, &rule);
        if applied > 0 {
            current.updated_at = now();
            state.tasks.insert(current.id.clone(), current);
            if let Some(mut stored) = state.automation_rules.get_mut(rule.id.as_str()) {
                stored.run_count += 1;
                stored.last_run_at = Some(now());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Task handlers
// ---------------------------------------------------------------------------

async fn list_project_tasks(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<ProjectTask>> = (|| {
        let project = state.get_project(path.as_str())?;
        let (viewer_id, viewer_role) = viewer_of(&params);
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        let sort = params.allowed_sort(
            &[
                "created_at",
                "updated_at",
                "due_date",
                "priority",
                "estimate_points",
            ],
            "created_at",
        );
        let mut items: Vec<ProjectTask> = state
            .tasks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|task| {
                task.project_id == project.id
                    && matches_search(&format!("{} {}", task.title, task.description), &params)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| task.status == *status)
                    && params.filters.get("assignee_id").is_none_or(|assignee| {
                        task.assignee_id.as_deref() == Some(assignee.as_str())
                    })
                    && params
                        .filters
                        .get("reporter_id")
                        .is_none_or(|reporter| task.reporter_id == *reporter)
                    && params
                        .filters
                        .get("task_type")
                        .is_none_or(|task_type| task.task_type == *task_type)
                    && params
                        .filters
                        .get("priority")
                        .is_none_or(|priority| task.priority == *priority)
                    && params.filters.get("label").is_none_or(|label| {
                        task.labels.iter().any(|task_label| task_label == label)
                    })
                    && params.filters.get("sprint_id").is_none_or(|sprint| {
                        task.sprint_id.as_deref() == Some(sprint.as_str())
                    })
                    && params.filters.get("phase_id").is_none_or(|phase| {
                        task.phase_id.as_deref() == Some(phase.as_str())
                    })
                    && params.filters.get("milestone_id").is_none_or(|milestone| {
                        task.milestone_id.as_deref() == Some(milestone.as_str())
                    })
                    && params.filters.get("parent_id").is_none_or(|parent| {
                        task.parent_id.as_deref() == Some(parent.as_str())
                    })
                    && params.filters.get("epic_id").is_none_or(|epic| {
                        task.epic_id.as_deref() == Some(epic.as_str())
                    })
            })
            .collect();
        s3_sort_tasks(&mut items, &sort, params.normalized().order == "desc");
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn create_project_task(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    body: web::Json<CreateProjectTask>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTask> = (|| {
        let input = body.into_inner();
        let viewer = query.into_inner();
        let project = state.get_project(path.as_str())?;
        require_capability(&state, viewer.viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        let title = input.title.trim().to_string();
        if title.is_empty() {
            return Err(UnifiedError::bad_request("Task title must not be empty"));
        }
        let task_type = input.task_type.unwrap_or_else(|| "task".to_string());
        if !s3_valid_task_type(&task_type) {
            return Err(UnifiedError::bad_request(
                "task_type must be one of: task, story, bug, spike, feature, enabler, chore",
            ));
        }
        let priority = input.priority.unwrap_or_else(|| "normal".to_string());
        if !s3_valid_priority(&priority) {
            return Err(UnifiedError::bad_request(
                "priority must be one of: low, normal, high, urgent",
            ));
        }
        let status = match input.status {
            Some(status) => {
                if !s3_methodology_allows_status(&project, &status) {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown status for this project methodology: {status}"
                    )));
                }
                status
            }
            None => s3_default_status(&project),
        };
        if let Some(points) = input.estimate_points {
            if points < 0.0 {
                return Err(UnifiedError::bad_request(
                    "estimate_points must not be negative",
                ));
            }
        }
        if let Some(hours) = input.estimate_hours {
            if hours < 0.0 {
                return Err(UnifiedError::bad_request(
                    "estimate_hours must not be negative",
                ));
            }
        }
        if let Some(parent_id) = &input.parent_id {
            let parent = state.get_task(parent_id.as_str())?;
            if parent.project_id != project.id {
                return Err(UnifiedError::bad_request(
                    "parent_id must reference a task in the same project",
                ));
            }
        }
        if let Some(epic_id) = &input.epic_id {
            let epic = state.get_task(epic_id.as_str())?;
            if epic.project_id != project.id {
                return Err(UnifiedError::bad_request(
                    "epic_id must reference a task in the same project",
                ));
            }
            if epic.task_type != "feature" && epic.task_type != "epic" {
                return Err(UnifiedError::bad_request(
                    "epic_id must reference a feature or epic task",
                ));
            }
        }
        s3_validate_task_refs(
            &state,
            &project.id,
            input.board_id.as_deref(),
            input.sprint_id.as_deref(),
            input.phase_id.as_deref(),
            input.cycle_id.as_deref(),
            input.pi_id.as_deref(),
            input.milestone_id.as_deref(),
        )?;
        let reporter_id = input
            .reporter_id
            .and_then(|reporter| {
                let reporter = reporter.trim().to_string();
                if reporter.is_empty() {
                    None
                } else {
                    Some(reporter)
                }
            })
            .or_else(|| viewer.viewer_id.clone())
            .unwrap_or_else(|| "system".to_string());
        let custom_values = if input.custom_values.is_null() {
            json!({})
        } else {
            input.custom_values
        };
        let task = ProjectTask {
            id: new_id(),
            project_id: project.id.clone(),
            board_id: input.board_id,
            parent_id: input.parent_id,
            epic_id: input.epic_id,
            title,
            description: input.description.unwrap_or_default(),
            task_type,
            status,
            priority,
            assignee_id: input.assignee_id,
            reporter_id,
            estimate_points: input.estimate_points,
            estimate_hours: input.estimate_hours,
            labels: input.labels,
            custom_values,
            start_date: input.start_date,
            due_date: input.due_date,
            completed_at: None,
            sprint_id: input.sprint_id,
            phase_id: input.phase_id,
            cycle_id: input.cycle_id,
            pi_id: input.pi_id,
            milestone_id: input.milestone_id,
            process_group: input.process_group,
            knowledge_area: input.knowledge_area,
            waste_flag: input.waste_flag,
            pair_partner_id: input.pair_partner_id,
            test_first: input.test_first.unwrap_or(false),
            dmaic_phase: input.dmaic_phase,
            appetite_weeks: input.appetite_weeks,
            created_at: now(),
            updated_at: now(),
        };
        state.tasks.insert(task.id.clone(), task.clone());
        apply_automation_rules(&state, &project, &task, "task_created");
        let stored = state.get_task(&task.id)?;
        let actor = viewer
            .viewer_id
            .as_deref()
            .unwrap_or(stored.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "task.created",
            "task",
            Some(stored.id.clone()),
            json!({
                "title": stored.title,
                "status": stored.status,
                "task_type": stored.task_type,
            }),
        );
        state.emit(
            "project.task.created",
            serde_json::to_value(&stored).unwrap_or(Value::Null),
        );
        if let Some(assignee_id) = &stored.assignee_id {
            state.notify(
                assignee_id,
                format!("Task assigned: {}", stored.title),
                format!(
                    "You were assigned to \"{}\" in project {}.",
                    stored.title, project.name
                ),
            );
        }
        Ok(stored)
    })();
    match result {
        Ok(task) => json::success_response(task, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn get_project_task(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTask> = (|| {
        let viewer = query.into_inner();
        let task = state.get_task(path.as_str())?;
        let project = state.get_project(&task.project_id)?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        Ok(task)
    })();
    json::respond(result, false)
}

async fn update_project_task(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateProjectTask>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTask> = (|| {
        let input = body.into_inner();
        let viewer = query.into_inner();
        let task_id = path.into_inner();
        let existing = state.get_task(&task_id)?;
        let project = state.get_project(&existing.project_id)?;
        require_capability(&state, viewer.viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        if let Some(title) = &input.title {
            if title.trim().is_empty() {
                return Err(UnifiedError::bad_request("Task title must not be empty"));
            }
        }
        if let Some(priority) = &input.priority {
            if !s3_valid_priority(priority) {
                return Err(UnifiedError::bad_request(
                    "priority must be one of: low, normal, high, urgent",
                ));
            }
        }
        if let Some(status) = &input.status {
            if !s3_methodology_allows_status(&project, status) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown status for this project methodology: {status}"
                )));
            }
        }
        if let Some(points) = input.estimate_points {
            if points < 0.0 {
                return Err(UnifiedError::bad_request(
                    "estimate_points must not be negative",
                ));
            }
        }
        if let Some(hours) = input.estimate_hours {
            if hours < 0.0 {
                return Err(UnifiedError::bad_request(
                    "estimate_hours must not be negative",
                ));
            }
        }
        s3_validate_task_refs(
            &state,
            &project.id,
            input.board_id.as_ref().and_then(|value| value.as_deref()),
            input.sprint_id.as_ref().and_then(|value| value.as_deref()),
            input.phase_id.as_ref().and_then(|value| value.as_deref()),
            input.cycle_id.as_ref().and_then(|value| value.as_deref()),
            input.pi_id.as_ref().and_then(|value| value.as_deref()),
            input.milestone_id.as_ref().and_then(|value| value.as_deref()),
        )?;
        let old_status = existing.status.clone();
        let old_assignee = existing.assignee_id.clone();
        let mut task = state
            .tasks
            .get_mut(task_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Task not found"))?;
        if let Some(title) = input.title {
            task.title = title.trim().to_string();
        }
        if let Some(description) = input.description {
            task.description = description;
        }
        if let Some(status) = input.status {
            task.status = status;
        }
        if let Some(priority) = input.priority {
            task.priority = priority;
        }
        if let Some(assignee_id) = input.assignee_id {
            task.assignee_id = assignee_id;
        }
        if let Some(points) = input.estimate_points {
            task.estimate_points = Some(points);
        }
        if let Some(hours) = input.estimate_hours {
            task.estimate_hours = Some(hours);
        }
        if let Some(labels) = input.labels {
            task.labels = labels;
        }
        if let Some(custom_values) = input.custom_values {
            task.custom_values = custom_values;
        }
        if let Some(start_date) = input.start_date {
            task.start_date = start_date;
        }
        if let Some(due_date) = input.due_date {
            task.due_date = due_date;
        }
        if let Some(sprint_id) = input.sprint_id {
            task.sprint_id = sprint_id;
        }
        if let Some(phase_id) = input.phase_id {
            task.phase_id = phase_id;
        }
        if let Some(cycle_id) = input.cycle_id {
            task.cycle_id = cycle_id;
        }
        if let Some(pi_id) = input.pi_id {
            task.pi_id = pi_id;
        }
        if let Some(milestone_id) = input.milestone_id {
            task.milestone_id = milestone_id;
        }
        if let Some(process_group) = input.process_group {
            task.process_group = process_group;
        }
        if let Some(knowledge_area) = input.knowledge_area {
            task.knowledge_area = knowledge_area;
        }
        if let Some(waste_flag) = input.waste_flag {
            task.waste_flag = Some(waste_flag);
        }
        if let Some(pair_partner_id) = input.pair_partner_id {
            task.pair_partner_id = pair_partner_id;
        }
        if let Some(test_first) = input.test_first {
            task.test_first = test_first;
        }
        if let Some(dmaic_phase) = input.dmaic_phase {
            task.dmaic_phase = dmaic_phase;
        }
        if let Some(appetite_weeks) = input.appetite_weeks {
            task.appetite_weeks = appetite_weeks;
        }
        if let Some(board_id) = input.board_id {
            task.board_id = board_id;
        }
        let status_changed = task.status != old_status;
        let became_done = status_changed && task.status == "done";
        let left_done = status_changed && old_status == "done";
        if became_done {
            task.completed_at = Some(now());
        } else if left_done {
            task.completed_at = None;
        }
        let assignee_changed = task.assignee_id != old_assignee;
        task.updated_at = now();
        let updated = task.clone();
        drop(task);
        if status_changed {
            apply_automation_rules(&state, &project, &updated, "status_changed");
            if became_done {
                apply_automation_rules(&state, &project, &updated, "task_completed");
            }
        }
        let final_task = state.get_task(&task_id)?;
        let actor = viewer
            .viewer_id
            .as_deref()
            .unwrap_or(final_task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "task.updated",
            "task",
            Some(task_id.clone()),
            json!({
                "status": final_task.status,
                "previous_status": old_status,
            }),
        );
        if assignee_changed {
            if let Some(assignee_id) = &final_task.assignee_id {
                state.emit(
                    "project.task.assigned",
                    serde_json::to_value(&final_task).unwrap_or(Value::Null),
                );
                state.notify(
                    assignee_id,
                    format!("Task assigned: {}", final_task.title),
                    format!(
                        "You were assigned to \"{}\" in project {}.",
                        final_task.title, project.name
                    ),
                );
            }
        }
        if became_done {
            state.emit(
                "project.task.completed",
                serde_json::to_value(&final_task).unwrap_or(Value::Null),
            );
            state.notify_task_watchers(
                &final_task,
                format!("Task completed: {}", final_task.title),
                format!(
                    "\"{}\" was completed in project {}.",
                    final_task.title, project.name
                ),
            );
        }
        Ok(final_task)
    })();
    json::respond(result, false)
}

async fn delete_project_task(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let viewer = query.into_inner();
        let task_id = path.into_inner();
        let task = state.get_task(&task_id)?;
        let project = state.get_project(&task.project_id)?;
        require_capability(&state, viewer.viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        let deleted_tasks = s3_delete_task_cascade(&state, &task_id);
        let actor = viewer
            .viewer_id
            .as_deref()
            .unwrap_or(task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "task.deleted",
            "task",
            Some(task_id.clone()),
            json!({
                "title": task.title,
                "deleted_tasks": deleted_tasks,
            }),
        );
        Ok(json!({
            "deleted": true,
            "id": task_id,
            "deleted_tasks": deleted_tasks,
        }))
    })();
    json::respond(result, false)
}

async fn move_project_task(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    body: web::Json<MoveTask>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTask> = (|| {
        let input = body.into_inner();
        let viewer = query.into_inner();
        let task_id = path.into_inner();
        let existing = state.get_task(&task_id)?;
        let project = state.get_project(&existing.project_id)?;
        require_capability(&state, viewer.viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        let target_board = match &input.board_id {
            Some(board_id) => {
                s3_require_board_in_project(&state, &project.id, board_id.as_str())?;
                Some(board_id.clone())
            }
            None => existing.board_id.clone(),
        };
        let target_status = match &input.column_status_key {
            Some(column_key) => {
                let column = s3_find_column(
                    &state,
                    &project.id,
                    target_board.as_deref(),
                    column_key.as_str(),
                )
                .ok_or_else(|| {
                    UnifiedError::bad_request(format!("Unknown column: {column_key}"))
                })?;
                column.status_key
            }
            None => match &input.status {
                Some(status) => status.clone(),
                None => {
                    return Err(UnifiedError::bad_request(
                        "Provide status or column_status_key to move the task",
                    ));
                }
            },
        };
        if !s3_methodology_allows_status(&project, &target_status) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown status for this project methodology: {target_status}"
            )));
        }
        s3_check_wip_limit(
            &state,
            &project.id,
            target_board.as_deref(),
            &target_status,
            &task_id,
        )?;
        let old_status = existing.status.clone();
        let mut task = state
            .tasks
            .get_mut(task_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Task not found"))?;
        let status_changed = task.status != target_status;
        let became_done = status_changed && target_status == "done";
        let left_done = status_changed && task.status == "done";
        task.board_id = target_board.clone();
        task.status = target_status.clone();
        if became_done {
            task.completed_at = Some(now());
        } else if left_done {
            task.completed_at = None;
        }
        task.updated_at = now();
        let updated = task.clone();
        drop(task);
        let actor = viewer
            .viewer_id
            .as_deref()
            .unwrap_or(updated.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "task.moved",
            "task",
            Some(task_id.clone()),
            json!({
                "from_status": old_status,
                "to_status": target_status,
                "board_id": target_board,
            }),
        );
        state.emit(
            "project.task.moved",
            json!({
                "task": serde_json::to_value(&updated).unwrap_or(Value::Null),
                "from_status": old_status,
                "to_status": target_status,
                "board_id": target_board,
            }),
        );
        if status_changed {
            state.notify_task_watchers(
                &updated,
                format!("Task moved: {}", updated.title),
                format!(
                    "\"{}\" moved to {} in project {}.",
                    updated.title, target_status, project.name
                ),
            );
        }
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn assign_project_task(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    body: web::Json<AssignTask>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTask> = (|| {
        let input = body.into_inner();
        let viewer = query.into_inner();
        let task_id = path.into_inner();
        let assignee_id = input.assignee_id.trim().to_string();
        if assignee_id.is_empty() {
            return Err(UnifiedError::bad_request("assignee_id must not be empty"));
        }
        let existing = state.get_task(&task_id)?;
        let project = state.get_project(&existing.project_id)?;
        require_capability(&state, viewer.viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        if !state.is_member(&project.id, &assignee_id) {
            return Err(UnifiedError::bad_request(
                "Assignee must be a project member",
            ));
        }
        let mut task = state
            .tasks
            .get_mut(task_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Task not found"))?;
        task.assignee_id = Some(assignee_id.clone());
        task.updated_at = now();
        let updated = task.clone();
        drop(task);
        let actor = viewer
            .viewer_id
            .as_deref()
            .unwrap_or(updated.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "task.assigned",
            "task",
            Some(task_id.clone()),
            json!({ "assignee_id": assignee_id }),
        );
        state.emit(
            "project.task.assigned",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        state.notify(
            &assignee_id,
            format!("Task assigned: {}", updated.title),
            format!(
                "You were assigned to \"{}\" in project {}.",
                updated.title, project.name
            ),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn complete_project_task(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTask> = (|| {
        let viewer = query.into_inner();
        let task_id = path.into_inner();
        let existing = state.get_task(&task_id)?;
        let project = state.get_project(&existing.project_id)?;
        require_capability(&state, viewer.viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        let already_done = existing.status == "done";
        let mut task = state
            .tasks
            .get_mut(task_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Task not found"))?;
        if !already_done {
            task.status = "done".to_string();
            task.completed_at = Some(now());
            task.updated_at = now();
        }
        let updated = task.clone();
        drop(task);
        if !already_done {
            apply_automation_rules(&state, &project, &updated, "task_completed");
        }
        let final_task = state.get_task(&task_id)?;
        if !already_done {
            let actor = viewer
                .viewer_id
                .as_deref()
                .unwrap_or(final_task.reporter_id.as_str());
            state.log_activity(
                &project.id,
                actor,
                "task.completed",
                "task",
                Some(task_id.clone()),
                json!({}),
            );
            state.emit(
                "project.task.completed",
                serde_json::to_value(&final_task).unwrap_or(Value::Null),
            );
            state.notify_task_watchers(
                &final_task,
                format!("Task completed: {}", final_task.title),
                format!(
                    "\"{}\" was completed in project {}.",
                    final_task.title, project.name
                ),
            );
        }
        Ok(final_task)
    })();
    json::respond(result, false)
}

async fn reopen_project_task(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTask> = (|| {
        let viewer = query.into_inner();
        let task_id = path.into_inner();
        let existing = state.get_task(&task_id)?;
        let project = state.get_project(&existing.project_id)?;
        require_capability(&state, viewer.viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        let target_status = s3_first_open_status(&project);
        let mut task = state
            .tasks
            .get_mut(task_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Task not found"))?;
        task.status = target_status.clone();
        task.completed_at = None;
        task.updated_at = now();
        let updated = task.clone();
        drop(task);
        let actor = viewer
            .viewer_id
            .as_deref()
            .unwrap_or(updated.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "task.reopened",
            "task",
            Some(task_id.clone()),
            json!({ "status": target_status }),
        );
        state.emit(
            "project.task.reopened",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        state.notify_task_watchers(
            &updated,
            format!("Task reopened: {}", updated.title),
            format!(
                "\"{}\" was reopened in project {}.",
                updated.title, project.name
            ),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Subtasks
// ---------------------------------------------------------------------------

async fn list_task_subtasks(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<ProjectTask>> = (|| {
        let task = state.get_task(path.as_str())?;
        let project = state.get_project(&task.project_id)?;
        let (viewer_id, viewer_role) = viewer_of(&params);
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        let sort = params.allowed_sort(
            &["created_at", "updated_at", "due_date", "priority"],
            "created_at",
        );
        let mut items: Vec<ProjectTask> = state
            .tasks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|child| {
                child.parent_id.as_deref() == Some(task.id.as_str())
                    && matches_search(&format!("{} {}", child.title, child.description), &params)
            })
            .collect();
        s3_sort_tasks(&mut items, &sort, params.normalized().order == "desc");
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Dependencies
// ---------------------------------------------------------------------------

async fn list_task_dependencies(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let viewer = query.into_inner();
        let task = state.get_task(path.as_str())?;
        let project = state.get_project(&task.project_id)?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        let mut outgoing: Vec<ProjectTaskDependency> = state
            .dependencies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|dependency| dependency.task_id == task.id)
            .collect();
        let mut incoming: Vec<ProjectTaskDependency> = state
            .dependencies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|dependency| dependency.depends_on_task_id == task.id)
            .collect();
        outgoing.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        incoming.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(json!({
            "task_id": task.id,
            "outgoing": outgoing,
            "incoming": incoming,
        }))
    })();
    json::respond(result, false)
}

async fn add_task_dependency(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    body: web::Json<AddTaskDependency>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<(ProjectTaskDependency, bool)> = (|| {
        let input = body.into_inner();
        let viewer = query.into_inner();
        let task_id = path.into_inner();
        let task = state.get_task(&task_id)?;
        let project = state.get_project(&task.project_id)?;
        require_capability(&state, viewer.viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        let depends_on_id = input.depends_on_task_id.trim().to_string();
        if depends_on_id.is_empty() {
            return Err(UnifiedError::bad_request(
                "depends_on_task_id must not be empty",
            ));
        }
        if depends_on_id == task_id {
            return Err(UnifiedError::bad_request(
                "A task cannot depend on itself",
            ));
        }
        let kind = input.kind.unwrap_or_else(|| "finish_start".to_string());
        if !s3_valid_dependency_kind(&kind) {
            return Err(UnifiedError::bad_request(
                "kind must be one of: finish_start, start_start, finish_finish, start_finish, blocks, related",
            ));
        }
        let depends_on = state.get_task(&depends_on_id)?;
        if depends_on.project_id != project.id {
            return Err(UnifiedError::bad_request(
                "depends_on_task_id must reference a task in the same project",
            ));
        }
        // Cycle rejection: walk from the dependency target following
        // depends_on edges; reaching this task would close a loop.
        let mut visited = std::collections::HashSet::new();
        let mut stack = vec![depends_on_id.clone()];
        while let Some(current) = stack.pop() {
            if current == task_id {
                return Err(UnifiedError::bad_request(
                    "This dependency would create a dependency cycle",
                ));
            }
            if !visited.insert(current.clone()) {
                continue;
            }
            for entry in state.dependencies.iter() {
                if entry.value().task_id == current {
                    stack.push(entry.value().depends_on_task_id.clone());
                }
            }
        }
        if let Some(existing) = state
            .dependencies
            .iter()
            .map(|entry| entry.value().clone())
            .find(|dependency| {
                dependency.task_id == task_id
                    && dependency.depends_on_task_id == depends_on_id
                    && dependency.kind == kind
            })
        {
            return Ok((existing, false));
        }
        let dependency = ProjectTaskDependency {
            id: new_id(),
            task_id: task_id.clone(),
            depends_on_task_id: depends_on_id,
            kind,
            lag_days: input.lag_days.unwrap_or(0),
            created_at: now(),
        };
        state
            .dependencies
            .insert(dependency.id.clone(), dependency.clone());
        let actor = viewer
            .viewer_id
            .as_deref()
            .unwrap_or(task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "dependency.added",
            "dependency",
            Some(dependency.id.clone()),
            json!({
                "task_id": task_id,
                "depends_on_task_id": dependency.depends_on_task_id,
                "kind": dependency.kind,
            }),
        );
        Ok((dependency, true))
    })();
    match result {
        Ok((dependency, is_new)) => {
            json::success_response(dependency, if is_new { 201 } else { 200 })
        }
        Err(error) => json::error_response(&error, false),
    }
}

async fn remove_task_dependency(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let viewer = query.into_inner();
        let (task_id, dependency_id) = path.into_inner();
        let task = state.get_task(&task_id)?;
        let project = state.get_project(&task.project_id)?;
        require_capability(&state, viewer.viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        let dependency = state
            .dependencies
            .get(dependency_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Dependency not found"))?;
        if dependency.task_id != task_id {
            return Err(UnifiedError::not_found("Dependency not found"));
        }
        state.dependencies.remove(dependency_id.as_str());
        let actor = viewer
            .viewer_id
            .as_deref()
            .unwrap_or(task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "dependency.removed",
            "dependency",
            Some(dependency_id.clone()),
            json!({ "task_id": task_id }),
        );
        Ok(json!({ "deleted": true, "id": dependency_id }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Comments
// ---------------------------------------------------------------------------

async fn list_task_comments(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<ProjectTaskComment>> = (|| {
        let task = state.get_task(path.as_str())?;
        let project = state.get_project(&task.project_id)?;
        let (viewer_id, viewer_role) = viewer_of(&params);
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        let sort = params.allowed_sort(&["created_at", "updated_at"], "created_at");
        let mut items: Vec<ProjectTaskComment> = state
            .comments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|comment| {
                comment.task_id == task.id && matches_search(&comment.body, &params)
            })
            .collect();
        match sort.as_str() {
            "updated_at" => items.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn add_task_comment(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    body: web::Json<AddTaskComment>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTaskComment> = (|| {
        let input = body.into_inner();
        let viewer = query.into_inner();
        let task_id = path.into_inner();
        let task = state.get_task(&task_id)?;
        let project = state.get_project(&task.project_id)?;
        require_capability(&state, viewer.viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        let body_text = input.body.trim().to_string();
        if body_text.is_empty() {
            return Err(UnifiedError::bad_request("Comment body must not be empty"));
        }
        let author_id = input.author_id.trim().to_string();
        if author_id.is_empty() {
            return Err(UnifiedError::bad_request("author_id must not be empty"));
        }
        let comment = ProjectTaskComment {
            id: new_id(),
            task_id: task_id.clone(),
            author_id: author_id.clone(),
            body: body_text,
            created_at: now(),
            updated_at: now(),
        };
        state
            .comments
            .insert(comment.id.clone(), comment.clone());
        let actor = viewer
            .viewer_id
            .as_deref()
            .unwrap_or(task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "task.commented",
            "comment",
            Some(comment.id.clone()),
            json!({ "task_id": task_id }),
        );
        state.emit(
            "project.task.commented",
            json!({
                "task": serde_json::to_value(&task).unwrap_or(Value::Null),
                "comment": serde_json::to_value(&comment).unwrap_or(Value::Null),
            }),
        );
        state.notify_task_watchers(
            &task,
            format!("New comment on: {}", task.title),
            format!(
                "{} commented on \"{}\" in project {}.",
                author_id, task.title, project.name
            ),
        );
        if let Some(assignee_id) = &task.assignee_id {
            if assignee_id != &author_id {
                state.notify(
                    assignee_id,
                    format!("New comment on: {}", task.title),
                    format!(
                        "{} commented on \"{}\" in project {}.",
                        author_id, task.title, project.name
                    ),
                );
            }
        }
        Ok(comment)
    })();
    match result {
        Ok(comment) => json::success_response(comment, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn update_task_comment(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateTaskComment>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTaskComment> = (|| {
        let input = body.into_inner();
        let viewer = query.into_inner();
        let (task_id, comment_id) = path.into_inner();
        let task = state.get_task(&task_id)?;
        let project = state.get_project(&task.project_id)?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        let comment = state
            .comments
            .get(comment_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Comment not found"))?;
        if comment.task_id != task_id {
            return Err(UnifiedError::not_found("Comment not found"));
        }
        // Only the author may edit, verified against the viewer id.
        match viewer.viewer_id.as_deref() {
            Some(viewer_id) if viewer_id == comment.author_id.as_str() => {}
            _ => {
                return Err(UnifiedError::forbidden(
                    "Only the comment author may edit this comment",
                ));
            }
        }
        let body_text = input.body.trim().to_string();
        if body_text.is_empty() {
            return Err(UnifiedError::bad_request("Comment body must not be empty"));
        }
        let mut stored = state
            .comments
            .get_mut(comment_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Comment not found"))?;
        stored.body = body_text;
        stored.updated_at = now();
        let updated = stored.clone();
        drop(stored);
        let actor = viewer.viewer_id.as_deref().unwrap_or(task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "comment.updated",
            "comment",
            Some(comment_id.clone()),
            json!({ "task_id": task_id }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_task_comment(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let viewer = query.into_inner();
        let (task_id, comment_id) = path.into_inner();
        let task = state.get_task(&task_id)?;
        let project = state.get_project(&task.project_id)?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        let comment = state
            .comments
            .get(comment_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Comment not found"))?;
        if comment.task_id != task_id {
            return Err(UnifiedError::not_found("Comment not found"));
        }
        // The author may delete their own comment; anyone else needs the
        // project management capability.
        let is_author = viewer.viewer_id.as_deref() == Some(comment.author_id.as_str());
        if !is_author {
            require_capability(&state, viewer.viewer_role.as_deref(), "projects:manage")?;
        }
        state.comments.remove(comment_id.as_str());
        let actor = viewer
            .viewer_id
            .as_deref()
            .unwrap_or(task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "comment.deleted",
            "comment",
            Some(comment_id.clone()),
            json!({ "task_id": task_id }),
        );
        Ok(json!({ "deleted": true, "id": comment_id }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Watchers
// ---------------------------------------------------------------------------

async fn list_task_watchers(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<ProjectTaskWatcher>> = (|| {
        let task = state.get_task(path.as_str())?;
        let project = state.get_project(&task.project_id)?;
        let (viewer_id, viewer_role) = viewer_of(&params);
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        let sort = params.allowed_sort(&["created_at"], "created_at");
        let mut items: Vec<ProjectTaskWatcher> = state
            .watchers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|watcher| watcher.task_id == task.id)
            .collect();
        match sort.as_str() {
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn add_task_watcher(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    body: web::Json<AddTaskWatcher>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<(ProjectTaskWatcher, bool)> = (|| {
        let input = body.into_inner();
        let viewer = query.into_inner();
        let task_id = path.into_inner();
        let user_id = input.user_id.trim().to_string();
        if user_id.is_empty() {
            return Err(UnifiedError::bad_request("user_id must not be empty"));
        }
        let task = state.get_task(&task_id)?;
        let project = state.get_project(&task.project_id)?;
        require_capability(&state, viewer.viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        // Idempotent: adding the same watcher twice returns the record
        // that already exists.
        if let Some(existing) = state
            .watchers
            .iter()
            .map(|entry| entry.value().clone())
            .find(|watcher| watcher.task_id == task_id && watcher.user_id == user_id)
        {
            return Ok((existing, false));
        }
        let watcher = ProjectTaskWatcher {
            id: new_id(),
            task_id: task_id.clone(),
            user_id: user_id.clone(),
            created_at: now(),
        };
        state.watchers.insert(watcher.id.clone(), watcher.clone());
        let actor = viewer
            .viewer_id
            .as_deref()
            .unwrap_or(task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "watcher.added",
            "watcher",
            Some(watcher.id.clone()),
            json!({ "task_id": task_id, "user_id": user_id }),
        );
        Ok((watcher, true))
    })();
    match result {
        Ok((watcher, is_new)) => {
            json::success_response(watcher, if is_new { 201 } else { 200 })
        }
        Err(error) => json::error_response(&error, false),
    }
}

async fn remove_task_watcher(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let viewer = query.into_inner();
        let (task_id, watcher_id) = path.into_inner();
        let task = state.get_task(&task_id)?;
        let project = state.get_project(&task.project_id)?;
        require_capability(&state, viewer.viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        let watcher = state
            .watchers
            .get(watcher_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Watcher not found"))?;
        if watcher.task_id != task_id {
            return Err(UnifiedError::not_found("Watcher not found"));
        }
        state.watchers.remove(watcher_id.as_str());
        let actor = viewer
            .viewer_id
            .as_deref()
            .unwrap_or(task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "watcher.removed",
            "watcher",
            Some(watcher_id.clone()),
            json!({ "task_id": task_id }),
        );
        Ok(json!({ "deleted": true, "id": watcher_id }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Attachments (storage references only; bytes live in the Storage module)
// ---------------------------------------------------------------------------

async fn list_task_attachments(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<ProjectTaskAttachment>> = (|| {
        let task = state.get_task(path.as_str())?;
        let project = state.get_project(&task.project_id)?;
        let (viewer_id, viewer_role) = viewer_of(&params);
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        let sort = params.allowed_sort(&["uploaded_at", "filename", "size_bytes"], "uploaded_at");
        let mut items: Vec<ProjectTaskAttachment> = state
            .attachments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|attachment| {
                attachment.task_id == task.id
                    && matches_search(
                        &format!("{} {}", attachment.filename, attachment.key),
                        &params,
                    )
            })
            .collect();
        match sort.as_str() {
            "filename" => items.sort_by(|a, b| a.filename.cmp(&b.filename)),
            "size_bytes" => items.sort_by(|a, b| {
                a.size_bytes
                    .unwrap_or(0)
                    .cmp(&b.size_bytes.unwrap_or(0))
            }),
            _ => items.sort_by(|a, b| a.uploaded_at.cmp(&b.uploaded_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn add_task_attachment(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    body: web::Json<AddTaskAttachment>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTaskAttachment> = (|| {
        let input = body.into_inner();
        let viewer = query.into_inner();
        let task_id = path.into_inner();
        let task = state.get_task(&task_id)?;
        let project = state.get_project(&task.project_id)?;
        require_capability(&state, viewer.viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        let target = input.target.trim().to_string();
        if target.is_empty() {
            return Err(UnifiedError::bad_request("target must not be empty"));
        }
        let key = input.key.trim().to_string();
        if key.is_empty() {
            return Err(UnifiedError::bad_request("key must not be empty"));
        }
        let uploaded_by = input.uploaded_by.trim().to_string();
        if uploaded_by.is_empty() {
            return Err(UnifiedError::bad_request("uploaded_by must not be empty"));
        }
        let stored_key = input
            .stored_key
            .and_then(|stored_key| {
                let stored_key = stored_key.trim().to_string();
                if stored_key.is_empty() {
                    None
                } else {
                    Some(stored_key)
                }
            })
            .unwrap_or_else(|| key.clone());
        let filename = input
            .filename
            .and_then(|filename| {
                let filename = filename.trim().to_string();
                if filename.is_empty() {
                    None
                } else {
                    Some(filename)
                }
            })
            .unwrap_or_else(|| key.clone());
        // Only the storage reference is stored here; the file itself must
        // already exist in the Storage module. No bytes are kept.
        let attachment = ProjectTaskAttachment {
            id: new_id(),
            task_id: task_id.clone(),
            target,
            key,
            stored_key,
            filename,
            content_type: input.content_type,
            size_bytes: input.size_bytes,
            uploaded_by,
            uploaded_at: now(),
        };
        state
            .attachments
            .insert(attachment.id.clone(), attachment.clone());
        let actor = viewer
            .viewer_id
            .as_deref()
            .unwrap_or(task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "attachment.added",
            "attachment",
            Some(attachment.id.clone()),
            json!({ "task_id": task_id, "filename": attachment.filename }),
        );
        Ok(attachment)
    })();
    match result {
        Ok(attachment) => json::success_response(attachment, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn delete_task_attachment(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let viewer = query.into_inner();
        let (task_id, attachment_id) = path.into_inner();
        let task = state.get_task(&task_id)?;
        let project = state.get_project(&task.project_id)?;
        require_capability(&state, viewer.viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        let attachment = state
            .attachments
            .get(attachment_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Attachment not found"))?;
        if attachment.task_id != task_id {
            return Err(UnifiedError::not_found("Attachment not found"));
        }
        state.attachments.remove(attachment_id.as_str());
        let actor = viewer
            .viewer_id
            .as_deref()
            .unwrap_or(task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "attachment.deleted",
            "attachment",
            Some(attachment_id.clone()),
            json!({ "task_id": task_id }),
        );
        Ok(json!({ "deleted": true, "id": attachment_id }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Automation rules
// ---------------------------------------------------------------------------

async fn list_automation_rules(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<ProjectAutomationRule>> = (|| {
        let project = state.get_project(path.as_str())?;
        let (viewer_id, viewer_role) = viewer_of(&params);
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        let sort = params.allowed_sort(&["created_at", "updated_at", "name"], "created_at");
        let mut items: Vec<ProjectAutomationRule> = state
            .automation_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| {
                rule.project_id == project.id
                    && matches_search(&rule.name, &params)
                    && params
                        .filters
                        .get("trigger")
                        .is_none_or(|trigger| rule.trigger == *trigger)
                    && params.filters.get("enabled").is_none_or(|flag| {
                        let wanted = flag == "true";
                        rule.enabled == wanted
                    })
            })
            .collect();
        match sort.as_str() {
            "updated_at" => items.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
            "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn create_automation_rule(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    body: web::Json<CreateAutomationRule>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectAutomationRule> = (|| {
        let input = body.into_inner();
        let viewer = query.into_inner();
        let project = state.get_project(path.as_str())?;
        require_capability(&state, viewer.viewer_role.as_deref(), "projects:manage")?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Err(UnifiedError::bad_request("Rule name must not be empty"));
        }
        if !s3_valid_rule_trigger(&input.trigger) {
            return Err(UnifiedError::bad_request(
                "trigger must be one of: task_created, status_changed, task_completed, due_soon, label_added",
            ));
        }
        let conditions = if input.conditions.is_null() {
            json!({})
        } else {
            input.conditions
        };
        s3_validate_rule_actions(&project, &input.actions)?;
        let rule = ProjectAutomationRule {
            id: new_id(),
            project_id: project.id.clone(),
            name,
            trigger: input.trigger,
            conditions,
            actions: input.actions,
            enabled: input.enabled.unwrap_or(true),
            run_count: 0,
            last_run_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state
            .automation_rules
            .insert(rule.id.clone(), rule.clone());
        let actor = viewer.viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project.id,
            actor,
            "automation_rule.created",
            "automation_rule",
            Some(rule.id.clone()),
            json!({ "name": rule.name, "trigger": rule.trigger }),
        );
        Ok(rule)
    })();
    match result {
        Ok(rule) => json::success_response(rule, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn get_automation_rule(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectAutomationRule> = (|| {
        let viewer = query.into_inner();
        let (project_id, rule_id) = path.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        let rule = state
            .automation_rules
            .get(rule_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Automation rule not found"))?;
        if rule.project_id != project_id {
            return Err(UnifiedError::not_found("Automation rule not found"));
        }
        Ok(rule)
    })();
    json::respond(result, false)
}

async fn update_automation_rule(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateAutomationRule>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectAutomationRule> = (|| {
        let input = body.into_inner();
        let viewer = query.into_inner();
        let (project_id, rule_id) = path.into_inner();
        let project = state.get_project(&project_id)?;
        require_capability(&state, viewer.viewer_role.as_deref(), "projects:manage")?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        let existing = state
            .automation_rules
            .get(rule_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Automation rule not found"))?;
        if existing.project_id != project_id {
            return Err(UnifiedError::not_found("Automation rule not found"));
        }
        if let Some(name) = &input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Rule name must not be empty"));
            }
        }
        if let Some(trigger) = &input.trigger {
            if !s3_valid_rule_trigger(trigger) {
                return Err(UnifiedError::bad_request(
                    "trigger must be one of: task_created, status_changed, task_completed, due_soon, label_added",
                ));
            }
        }
        if let Some(actions) = &input.actions {
            s3_validate_rule_actions(&project, actions)?;
        }
        let mut rule = state
            .automation_rules
            .get_mut(rule_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Automation rule not found"))?;
        if let Some(name) = input.name {
            rule.name = name.trim().to_string();
        }
        if let Some(trigger) = input.trigger {
            rule.trigger = trigger;
        }
        if let Some(conditions) = input.conditions {
            rule.conditions = if conditions.is_null() {
                json!({})
            } else {
                conditions
            };
        }
        if let Some(actions) = input.actions {
            rule.actions = actions;
        }
        if let Some(enabled) = input.enabled {
            rule.enabled = enabled;
        }
        rule.updated_at = now();
        let updated = rule.clone();
        drop(rule);
        let actor = viewer.viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project.id,
            actor,
            "automation_rule.updated",
            "automation_rule",
            Some(rule_id.clone()),
            json!({ "name": updated.name, "trigger": updated.trigger }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_automation_rule(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let viewer = query.into_inner();
        let (project_id, rule_id) = path.into_inner();
        let project = state.get_project(&project_id)?;
        require_capability(&state, viewer.viewer_role.as_deref(), "projects:manage")?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        let rule = state
            .automation_rules
            .get(rule_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Automation rule not found"))?;
        if rule.project_id != project_id {
            return Err(UnifiedError::not_found("Automation rule not found"));
        }
        state.automation_rules.remove(rule_id.as_str());
        let actor = viewer.viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project.id,
            actor,
            "automation_rule.deleted",
            "automation_rule",
            Some(rule_id.clone()),
            json!({ "name": rule.name }),
        );
        Ok(json!({ "deleted": true, "id": rule_id }))
    })();
    json::respond(result, false)
}

async fn run_automation_rule(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let viewer = query.into_inner();
        let (project_id, rule_id) = path.into_inner();
        let project = state.get_project(&project_id)?;
        require_capability(&state, viewer.viewer_role.as_deref(), "projects:manage")?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        let rule = state
            .automation_rules
            .get(rule_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Automation rule not found"))?;
        if rule.project_id != project_id {
            return Err(UnifiedError::not_found("Automation rule not found"));
        }
        // A manual run evaluates the rule against every task in the
        // project right now, whether or not the rule is enabled: the
        // explicit invocation is the trigger.
        let mut matched: u64 = 0;
        let mut applied: u64 = 0;
        let tasks: Vec<ProjectTask> = state
            .tasks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|task| task.project_id == project.id)
            .collect();
        for mut task in tasks {
            if !s3_rule_conditions_match(&task, &rule.conditions) {
                continue;
            }
            matched += 1;
            let applied_actions = s3_apply_rule_actions(&state, &project, &mut task, &rule);
            if applied_actions > 0 {
                task.updated_at = now();
                state.tasks.insert(task.id.clone(), task);
                applied += 1;
            }
        }
        if let Some(mut stored) = state.automation_rules.get_mut(rule_id.as_str()) {
            stored.run_count += 1;
            stored.last_run_at = Some(now());
        }
        let actor = viewer.viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project.id,
            actor,
            "automation_rule.ran",
            "automation_rule",
            Some(rule_id.clone()),
            json!({ "matched": matched, "applied": applied }),
        );
        Ok(json!({
            "rule_id": rule_id,
            "matched": matched,
            "applied": applied,
        }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Ticket bridge
// ---------------------------------------------------------------------------

async fn convert_task_to_ticket(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    body: web::Json<ConvertTaskToTicket>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let input = body.into_inner();
        let viewer = query.into_inner();
        let task = state.get_task(path.as_str())?;
        let project = state.get_project(&task.project_id)?;
        require_capability(&state, viewer.viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        state.emit(
            "ticket.create_requested",
            json!({
                "title": task.title,
                "body": task.description,
                "user_id": task.reporter_id,
                "category": "project-task",
                "department_id": input.department_id,
                "project_id": project.id,
                "task_id": task.id,
            }),
        );
        let actor = viewer
            .viewer_id
            .as_deref()
            .unwrap_or(task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "task.converted_to_ticket",
            "task",
            Some(task.id.clone()),
            json!({ "department_id": input.department_id }),
        );
        Ok(json!({ "requested": true, "task_id": task.id }))
    })();
    json::respond(result, false)
}

async fn create_task_from_ticket(
    state: web::Data<Arc<ProjectsState>>,
    body: web::Json<TaskFromTicket>,
    query: web::Query<S3ViewerQuery>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTask> = (|| {
        let input = body.into_inner();
        let viewer = query.into_inner();
        let project = state.get_project(&input.project_id)?;
        require_capability(&state, viewer.viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer.viewer_id.as_deref(),
            viewer.viewer_role.as_deref(),
        )?;
        let title = input.title.trim().to_string();
        if title.is_empty() {
            return Err(UnifiedError::bad_request("Task title must not be empty"));
        }
        let user_id = input.user_id.trim().to_string();
        if user_id.is_empty() {
            return Err(UnifiedError::bad_request("user_id must not be empty"));
        }
        let task = ProjectTask {
            id: new_id(),
            project_id: project.id.clone(),
            board_id: None,
            parent_id: None,
            epic_id: None,
            title,
            description: input.body.unwrap_or_default(),
            task_type: "task".to_string(),
            status: s3_default_status(&project),
            priority: "normal".to_string(),
            assignee_id: None,
            reporter_id: user_id,
            estimate_points: None,
            estimate_hours: None,
            labels: Vec::new(),
            custom_values: json!({}),
            start_date: None,
            due_date: None,
            completed_at: None,
            sprint_id: None,
            phase_id: None,
            cycle_id: None,
            pi_id: None,
            milestone_id: None,
            process_group: None,
            knowledge_area: None,
            waste_flag: None,
            pair_partner_id: None,
            test_first: false,
            dmaic_phase: None,
            appetite_weeks: None,
            created_at: now(),
            updated_at: now(),
        };
        state.tasks.insert(task.id.clone(), task.clone());
        apply_automation_rules(&state, &project, &task, "task_created");
        let stored = state.get_task(&task.id)?;
        let actor = viewer
            .viewer_id
            .as_deref()
            .unwrap_or(stored.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "task.created",
            "task",
            Some(stored.id.clone()),
            json!({
                "title": stored.title,
                "ticket_id": input.ticket_id,
                "department_id": input.department_id,
            }),
        );
        state.emit(
            "project.task.created",
            serde_json::to_value(&stored).unwrap_or(Value::Null),
        );
        Ok(stored)
    })();
    match result {
        Ok(task) => json::success_response(task, 201),
        Err(error) => json::error_response(&error, false),
    }
}
// Section s4 of the Projects module: methodology execution containers.
//
// Sprints (Scrum / Scrumban / Agile), phases (Waterfall / PRINCE2 stages /
// DMAIC / Hybrid), Shape Up cycles, SAFe program increments, and PRINCE2
// business cases. Imports, entity structs, the state type, the shared
// helpers (`now`, `new_id`, `paginate`, `viewer_of`, `matches_search`,
// `require_capability`, `require_project_access`, `methodology_profile`)
// and the route table live in section s1; this file only adds the input
// structs and handlers named there.

// ---------------------------------------------------------------------------
// Input structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProjectSprint {
    pub name: String,
    pub goal: Option<String>,
    pub status: Option<String>,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
    pub velocity_planned: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProjectSprint {
    pub name: Option<String>,
    pub goal: Option<String>,
    pub status: Option<String>,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
    pub velocity_planned: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddStandupNote {
    pub author_id: String,
    pub note: String,
    pub blockers: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SaveSprintNotes {
    pub notes: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProjectPhase {
    pub name: String,
    pub kind: Option<String>,
    pub position: Option<u32>,
    pub status: Option<String>,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProjectPhase {
    pub name: Option<String>,
    pub kind: Option<String>,
    pub position: Option<u32>,
    pub status: Option<String>,
    pub gate_status: Option<String>,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DecidePhaseGate {
    pub decision: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProjectCycle {
    pub name: String,
    pub appetite_weeks: Option<u32>,
    pub cooldown_weeks: Option<u32>,
    pub status: Option<String>,
    pub pitch: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProjectCycle {
    pub name: Option<String>,
    pub appetite_weeks: Option<u32>,
    pub cooldown_weeks: Option<u32>,
    pub status: Option<String>,
    pub pitch: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BetProjectCycle {
    pub decision: String,
    pub pitch: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProgramIncrement {
    pub name: String,
    pub status: Option<String>,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
    pub objectives: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProgramIncrement {
    pub name: Option<String>,
    pub status: Option<String>,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
    pub objectives: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlanProgramIncrement {
    #[serde(default)]
    pub objectives: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateBusinessCase {
    pub title: String,
    pub justification: Option<String>,
    pub benefits: Option<Value>,
    pub costs: Option<Value>,
    pub risks: Option<Value>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateBusinessCase {
    pub title: Option<String>,
    pub justification: Option<String>,
    pub benefits: Option<Value>,
    pub costs: Option<Value>,
    pub risks: Option<Value>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SaveExceptionReport {
    pub report: String,
}

// ---------------------------------------------------------------------------
// Section-private helpers (s4_ prefix; nothing here is defined in s1)
// ---------------------------------------------------------------------------

/// Trimmed, non-empty text, or `None` when the input is absent or blank.
fn s4_clean_text(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// Require a non-empty value; returns the trimmed text.
fn s4_require_text(value: &str, label: &str) -> UnifiedResult<String> {
    let text = value.trim().to_string();
    if text.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "{label} must not be empty"
        )));
    }
    Ok(text)
}

fn s4_valid_sprint_status(status: &str) -> bool {
    matches!(status, "planned" | "active" | "completed")
}

fn s4_valid_phase_kind(kind: &str) -> bool {
    matches!(kind, "waterfall" | "prince2_stage" | "dmaic" | "hybrid")
}

fn s4_valid_phase_status(status: &str) -> bool {
    matches!(status, "planned" | "active" | "completed")
}

fn s4_valid_gate_status(status: &str) -> bool {
    matches!(status, "pending" | "approved" | "rejected")
}

fn s4_valid_cycle_status(status: &str) -> bool {
    matches!(
        status,
        "shaping" | "betting" | "building" | "cooldown" | "done"
    )
}

fn s4_valid_pi_status(status: &str) -> bool {
    matches!(status, "planned" | "active" | "completed")
}

fn s4_valid_business_case_status(status: &str) -> bool {
    matches!(status, "draft" | "approved" | "rejected")
}

/// Capability check, project lookup, and access check in one step.
/// Returns the resolved project.
fn s4_resolve_project(
    state: &ProjectsState,
    project_id: &str,
    viewer_id: Option<&str>,
    viewer_role: Option<&str>,
    capability: &str,
) -> UnifiedResult<Project> {
    require_capability(state, viewer_role, capability)?;
    let project = state.get_project(project_id)?;
    require_project_access(state, &project, viewer_id, viewer_role)?;
    Ok(project)
}

/// Actor recorded in the activity log: the viewer, or `system` when
/// no viewer was supplied.
fn s4_actor(viewer_id: &Option<String>) -> &str {
    viewer_id.as_deref().unwrap_or("system")
}

fn s4_get_sprint(
    state: &ProjectsState,
    project_id: &str,
    sprint_id: &str,
) -> UnifiedResult<ProjectSprint> {
    let sprint = state
        .sprints
        .get(sprint_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Sprint not found"))?;
    if sprint.project_id != project_id {
        return Err(UnifiedError::not_found("Sprint not found"));
    }
    Ok(sprint)
}

fn s4_get_phase(
    state: &ProjectsState,
    project_id: &str,
    phase_id: &str,
) -> UnifiedResult<ProjectPhase> {
    let phase = state
        .phases
        .get(phase_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Phase not found"))?;
    if phase.project_id != project_id {
        return Err(UnifiedError::not_found("Phase not found"));
    }
    Ok(phase)
}

fn s4_get_cycle(
    state: &ProjectsState,
    project_id: &str,
    cycle_id: &str,
) -> UnifiedResult<ProjectCycle> {
    let cycle = state
        .cycles
        .get(cycle_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Cycle not found"))?;
    if cycle.project_id != project_id {
        return Err(UnifiedError::not_found("Cycle not found"));
    }
    Ok(cycle)
}

fn s4_get_program_increment(
    state: &ProjectsState,
    project_id: &str,
    pi_id: &str,
) -> UnifiedResult<ProjectProgramIncrement> {
    let pi = state
        .program_increments
        .get(pi_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Program increment not found"))?;
    if pi.project_id != project_id {
        return Err(UnifiedError::not_found("Program increment not found"));
    }
    Ok(pi)
}

fn s4_get_business_case(
    state: &ProjectsState,
    project_id: &str,
    case_id: &str,
) -> UnifiedResult<ProjectBusinessCase> {
    let case = state
        .business_cases
        .get(case_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Business case not found"))?;
    if case.project_id != project_id {
        return Err(UnifiedError::not_found("Business case not found"));
    }
    Ok(case)
}

/// Id of the active sprint in a project, ignoring `exclude_id` when
/// supplied (used when the sprint being changed is itself active).
fn s4_active_sprint_id(
    state: &ProjectsState,
    project_id: &str,
    exclude_id: Option<&str>,
) -> Option<String> {
    state
        .sprints
        .iter()
        .find(|entry| {
            let sprint = entry.value();
            sprint.project_id == project_id
                && sprint.status == "active"
                && exclude_id.is_none_or(|id| sprint.id != id)
        })
        .map(|entry| entry.value().id.clone())
}

/// Highest phase position in a project, or 0 when it has no phases.
fn s4_max_phase_position(state: &ProjectsState, project_id: &str) -> u32 {
    state
        .phases
        .iter()
        .filter(|entry| entry.value().project_id == project_id)
        .map(|entry| entry.value().position)
        .max()
        .unwrap_or(0)
}

/// Id of the next phase after `position` in the same project (lowest
/// position greater than the current one), if any.
fn s4_next_phase_id(state: &ProjectsState, project_id: &str, position: u32) -> Option<String> {
    state
        .phases
        .iter()
        .filter(|entry| {
            let phase = entry.value();
            phase.project_id == project_id && phase.position > position
        })
        .min_by_key(|entry| entry.value().position)
        .map(|entry| entry.value().id.clone())
}

fn s4_validate_date_range(
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    label: &str,
) -> UnifiedResult<()> {
    if let (Some(start), Some(end)) = (start, end) {
        if end <= start {
            return Err(UnifiedError::bad_request(format!(
                "{label} end date must be after the start date"
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Sprints
// ---------------------------------------------------------------------------

async fn list_project_sprints(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<PageResult<ProjectSprint>> = (|| {
        s4_resolve_project(
            &state,
            path.as_str(),
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:view",
        )?;
        let sort = params.allowed_sort(
            &["created_at", "name", "start_date", "end_date", "updated_at"],
            "created_at",
        );
        let mut items: Vec<ProjectSprint> = state
            .sprints
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|sprint| {
                sprint.project_id == path.as_str()
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| sprint.status == *status)
                    && matches_search(
                        &format!(
                            "{} {}",
                            sprint.name,
                            sprint.goal.as_deref().unwrap_or_default()
                        ),
                        &params,
                    )
            })
            .collect();
        match sort.as_str() {
            "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
            "start_date" => items.sort_by(|a, b| a.start_date.cmp(&b.start_date)),
            "end_date" => items.sort_by(|a, b| a.end_date.cmp(&b.end_date)),
            "updated_at" => items.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn create_project_sprint(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateProjectSprint>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectSprint> = (|| {
        let project = s4_resolve_project(
            &state,
            path.as_str(),
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
        )?;
        let input = body.into_inner();
        let name = s4_require_text(&input.name, "Sprint name")?;
        let profile = methodology_profile(&project.methodology);
        if !profile.is_some_and(|profile| profile.uses_sprints) {
            return Err(UnifiedError::bad_request(
                "methodology does not use sprints",
            ));
        }
        let status = input
            .status
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "planned".to_string());
        if !s4_valid_sprint_status(&status) {
            return Err(UnifiedError::bad_request(
                "Sprint status must be one of: planned, active, completed",
            ));
        }
        s4_validate_date_range(input.start_date, input.end_date, "Sprint")?;
        if status == "active"
            && s4_active_sprint_id(&state, &project.id, None).is_some()
        {
            return Err(UnifiedError::bad_request(
                "Project already has an active sprint",
            ));
        }
        let current = now();
        let sprint = ProjectSprint {
            id: new_id(),
            project_id: project.id.clone(),
            name,
            goal: s4_clean_text(input.goal),
            status,
            start_date: input.start_date,
            end_date: input.end_date,
            velocity_planned: input.velocity_planned,
            velocity_actual: None,
            standup_notes: Value::Array(Vec::new()),
            review_notes: None,
            retrospective_notes: None,
            created_at: current,
            updated_at: current,
        };
        state.sprints.insert(sprint.id.clone(), sprint.clone());
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "sprint.created",
            "sprint",
            Some(sprint.id.clone()),
            json!({ "name": sprint.name, "status": sprint.status }),
        );
        Ok(sprint)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn get_project_sprint(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, sprint_id) = path.into_inner();
    let result: UnifiedResult<ProjectSprint> = (|| {
        s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:view",
        )?;
        s4_get_sprint(&state, &project_id, &sprint_id)
    })();
    json::respond(result, false)
}

async fn update_project_sprint(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateProjectSprint>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, sprint_id) = path.into_inner();
    let result: UnifiedResult<ProjectSprint> = (|| {
        let project = s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
        )?;
        let input = body.into_inner();
        if let Some(status) = &input.status {
            if !s4_valid_sprint_status(status.trim()) {
                return Err(UnifiedError::bad_request(
                    "Sprint status must be one of: planned, active, completed",
                ));
            }
            if status.trim() == "active"
                && s4_active_sprint_id(&state, &project.id, Some(&sprint_id)).is_some()
            {
                return Err(UnifiedError::bad_request(
                    "Project already has an active sprint",
                ));
            }
        }
        let mut sprint = state
            .sprints
            .get_mut(sprint_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Sprint not found"))?;
        if sprint.project_id != project_id {
            return Err(UnifiedError::not_found("Sprint not found"));
        }
        let start_date = input.start_date.or(sprint.start_date);
        let end_date = input.end_date.or(sprint.end_date);
        s4_validate_date_range(start_date, end_date, "Sprint")?;
        if let Some(name) = input.name {
            sprint.name = s4_require_text(&name, "Sprint name")?;
        }
        if let Some(goal) = input.goal {
            sprint.goal = s4_clean_text(Some(goal));
        }
        if let Some(status) = input.status {
            sprint.status = status.trim().to_string();
        }
        sprint.start_date = start_date;
        sprint.end_date = end_date;
        if let Some(velocity_planned) = input.velocity_planned {
            sprint.velocity_planned = Some(velocity_planned);
        }
        sprint.updated_at = now();
        let updated = sprint.clone();
        drop(sprint);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "sprint.updated",
            "sprint",
            Some(updated.id.clone()),
            json!({ "name": updated.name, "status": updated.status }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_project_sprint(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, sprint_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let project = s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
        )?;
        let sprint = s4_get_sprint(&state, &project_id, &sprint_id)?;
        if sprint.status == "active" {
            return Err(UnifiedError::bad_request(
                "Cannot delete an active sprint",
            ));
        }
        // Return the sprint's tasks to the product backlog first so
        // no task keeps pointing at a sprint that no longer exists.
        for mut task in state.tasks.iter_mut() {
            if task.sprint_id.as_deref() == Some(sprint_id.as_str()) {
                task.sprint_id = None;
                task.updated_at = now();
            }
        }
        state
            .sprints
            .remove(sprint_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Sprint not found"))?;
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "sprint.deleted",
            "sprint",
            Some(sprint.id.clone()),
            json!({ "name": sprint.name }),
        );
        Ok(json!({ "deleted": true, "id": sprint_id }))
    })();
    json::respond(result, false)
}

async fn start_project_sprint(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, sprint_id) = path.into_inner();
    let result: UnifiedResult<ProjectSprint> = (|| {
        let project = s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
        )?;
        let sprint = s4_get_sprint(&state, &project_id, &sprint_id)?;
        if sprint.status != "planned" {
            return Err(UnifiedError::bad_request(
                "Only a planned sprint can be started",
            ));
        }
        if s4_active_sprint_id(&state, &project.id, Some(&sprint_id)).is_some() {
            return Err(UnifiedError::bad_request(
                "Project already has an active sprint",
            ));
        }
        let current = now();
        let mut sprint = state
            .sprints
            .get_mut(sprint_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Sprint not found"))?;
        sprint.status = "active".to_string();
        if sprint.start_date.is_none() {
            sprint.start_date = Some(current);
        }
        sprint.updated_at = current;
        let started = sprint.clone();
        drop(sprint);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "sprint.started",
            "sprint",
            Some(started.id.clone()),
            json!({ "name": started.name }),
        );
        state.emit(
            "project.sprint.started",
            json!({
                "project_id": project.id,
                "sprint_id": started.id,
                "name": started.name,
            }),
        );
        state.notify(
            &project.owner_id,
            format!("Sprint started: {}", started.name),
            format!("Sprint {} is now active.", started.name),
        );
        Ok(started)
    })();
    json::respond(result, false)
}

async fn close_project_sprint(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, sprint_id) = path.into_inner();
    let result: UnifiedResult<ProjectSprint> = (|| {
        let project = s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
        )?;
        let sprint = s4_get_sprint(&state, &project_id, &sprint_id)?;
        if sprint.status != "active" {
            return Err(UnifiedError::bad_request(
                "Only an active sprint can be closed",
            ));
        }
        // Delivered velocity: estimated points of the sprint's tasks
        // that reached done.
        let velocity_actual: f64 = state
            .tasks
            .iter()
            .filter(|entry| {
                let task = entry.value();
                task.sprint_id.as_deref() == Some(sprint_id.as_str()) && task.status == "done"
            })
            .map(|entry| entry.value().estimate_points.unwrap_or(0.0))
            .sum();
        let current = now();
        let mut sprint = state
            .sprints
            .get_mut(sprint_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Sprint not found"))?;
        sprint.status = "completed".to_string();
        sprint.velocity_actual = Some(velocity_actual);
        if sprint.end_date.is_none() {
            sprint.end_date = Some(current);
        }
        sprint.updated_at = current;
        let closed = sprint.clone();
        drop(sprint);
        // Unfinished work goes back to the product backlog.
        for mut task in state.tasks.iter_mut() {
            if task.sprint_id.as_deref() == Some(sprint_id.as_str()) && task.status != "done" {
                task.sprint_id = None;
                task.updated_at = now();
            }
        }
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "sprint.closed",
            "sprint",
            Some(closed.id.clone()),
            json!({ "name": closed.name, "velocity_actual": velocity_actual }),
        );
        state.emit(
            "project.sprint.closed",
            json!({
                "project_id": project.id,
                "sprint_id": closed.id,
                "name": closed.name,
                "velocity_actual": velocity_actual,
            }),
        );
        state.notify(
            &project.owner_id,
            format!("Sprint closed: {}", closed.name),
            format!(
                "Sprint {} is complete with a delivered velocity of {}.",
                closed.name, velocity_actual
            ),
        );
        Ok(closed)
    })();
    json::respond(result, false)
}

async fn add_sprint_standup(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<AddStandupNote>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, sprint_id) = path.into_inner();
    let result: UnifiedResult<ProjectSprint> = (|| {
        let project = s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
        )?;
        let input = body.into_inner();
        let author_id = s4_require_text(&input.author_id, "author_id")?;
        let note = s4_require_text(&input.note, "Standup note")?;
        let blockers = s4_clean_text(input.blockers);
        let mut sprint = state
            .sprints
            .get_mut(sprint_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Sprint not found"))?;
        if sprint.project_id != project_id {
            return Err(UnifiedError::not_found("Sprint not found"));
        }
        let mut notes = sprint
            .standup_notes
            .as_array()
            .cloned()
            .unwrap_or_default();
        notes.push(json!({
            "author_id": author_id,
            "note": note,
            "blockers": blockers,
            "at": now(),
        }));
        sprint.standup_notes = Value::Array(notes);
        sprint.updated_at = now();
        let updated = sprint.clone();
        drop(sprint);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "sprint.standup_added",
            "sprint",
            Some(updated.id.clone()),
            json!({ "name": updated.name, "author_id": author_id }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn save_sprint_review(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<SaveSprintNotes>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, sprint_id) = path.into_inner();
    let result: UnifiedResult<ProjectSprint> = (|| {
        let project = s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
        )?;
        let input = body.into_inner();
        let notes = s4_require_text(&input.notes, "Review notes")?;
        let mut sprint = state
            .sprints
            .get_mut(sprint_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Sprint not found"))?;
        if sprint.project_id != project_id {
            return Err(UnifiedError::not_found("Sprint not found"));
        }
        sprint.review_notes = Some(notes);
        sprint.updated_at = now();
        let updated = sprint.clone();
        drop(sprint);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "sprint.review_saved",
            "sprint",
            Some(updated.id.clone()),
            json!({ "name": updated.name }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn save_sprint_retrospective(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<SaveSprintNotes>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, sprint_id) = path.into_inner();
    let result: UnifiedResult<ProjectSprint> = (|| {
        let project = s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
        )?;
        let input = body.into_inner();
        let notes = s4_require_text(&input.notes, "Retrospective notes")?;
        let mut sprint = state
            .sprints
            .get_mut(sprint_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Sprint not found"))?;
        if sprint.project_id != project_id {
            return Err(UnifiedError::not_found("Sprint not found"));
        }
        sprint.retrospective_notes = Some(notes);
        sprint.updated_at = now();
        let updated = sprint.clone();
        drop(sprint);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "sprint.retrospective_saved",
            "sprint",
            Some(updated.id.clone()),
            json!({ "name": updated.name }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Phases
// ---------------------------------------------------------------------------

async fn list_project_phases(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<PageResult<ProjectPhase>> = (|| {
        s4_resolve_project(
            &state,
            path.as_str(),
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:view",
        )?;
        let sort = params.allowed_sort(
            &["position", "created_at", "name", "updated_at"],
            "position",
        );
        let mut items: Vec<ProjectPhase> = state
            .phases
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|phase| {
                phase.project_id == path.as_str()
                    && params
                        .filters
                        .get("kind")
                        .is_none_or(|kind| phase.kind == *kind)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| phase.status == *status)
                    && matches_search(&phase.name, &params)
            })
            .collect();
        match sort.as_str() {
            "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
            "created_at" => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
            "updated_at" => items.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
            _ => items.sort_by(|a, b| {
                a.position
                    .cmp(&b.position)
                    .then(a.created_at.cmp(&b.created_at))
            }),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn create_project_phase(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateProjectPhase>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectPhase> = (|| {
        let project = s4_resolve_project(
            &state,
            path.as_str(),
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
        )?;
        let input = body.into_inner();
        let name = s4_require_text(&input.name, "Phase name")?;
        let profile = methodology_profile(&project.methodology);
        if !profile.is_some_and(|profile| profile.uses_phases) {
            return Err(UnifiedError::bad_request(
                "methodology does not use phases",
            ));
        }
        let kind = input
            .kind
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "waterfall".to_string());
        if !s4_valid_phase_kind(&kind) {
            return Err(UnifiedError::bad_request(
                "Phase kind must be one of: waterfall, prince2_stage, dmaic, hybrid",
            ));
        }
        let status = input
            .status
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "planned".to_string());
        if !s4_valid_phase_status(&status) {
            return Err(UnifiedError::bad_request(
                "Phase status must be one of: planned, active, completed",
            ));
        }
        s4_validate_date_range(input.start_date, input.end_date, "Phase")?;
        let position = input
            .position
            .unwrap_or_else(|| s4_max_phase_position(&state, &project.id) + 1);
        let current = now();
        let phase = ProjectPhase {
            id: new_id(),
            project_id: project.id.clone(),
            name,
            kind,
            position,
            status,
            gate_status: "pending".to_string(),
            gate_decision: None,
            start_date: input.start_date,
            end_date: input.end_date,
            created_at: current,
            updated_at: current,
        };
        state.phases.insert(phase.id.clone(), phase.clone());
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "phase.created",
            "phase",
            Some(phase.id.clone()),
            json!({ "name": phase.name, "kind": phase.kind, "position": phase.position }),
        );
        Ok(phase)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn get_project_phase(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, phase_id) = path.into_inner();
    let result: UnifiedResult<ProjectPhase> = (|| {
        s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:view",
        )?;
        s4_get_phase(&state, &project_id, &phase_id)
    })();
    json::respond(result, false)
}

async fn update_project_phase(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateProjectPhase>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, phase_id) = path.into_inner();
    let result: UnifiedResult<ProjectPhase> = (|| {
        let project = s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
        )?;
        let input = body.into_inner();
        if let Some(kind) = &input.kind {
            if !s4_valid_phase_kind(kind.trim()) {
                return Err(UnifiedError::bad_request(
                    "Phase kind must be one of: waterfall, prince2_stage, dmaic, hybrid",
                ));
            }
        }
        if let Some(status) = &input.status {
            if !s4_valid_phase_status(status.trim()) {
                return Err(UnifiedError::bad_request(
                    "Phase status must be one of: planned, active, completed",
                ));
            }
        }
        if let Some(gate_status) = &input.gate_status {
            if !s4_valid_gate_status(gate_status.trim()) {
                return Err(UnifiedError::bad_request(
                    "Gate status must be one of: pending, approved, rejected",
                ));
            }
        }
        let mut phase = state
            .phases
            .get_mut(phase_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Phase not found"))?;
        if phase.project_id != project_id {
            return Err(UnifiedError::not_found("Phase not found"));
        }
        let start_date = input.start_date.or(phase.start_date);
        let end_date = input.end_date.or(phase.end_date);
        s4_validate_date_range(start_date, end_date, "Phase")?;
        if let Some(name) = input.name {
            phase.name = s4_require_text(&name, "Phase name")?;
        }
        if let Some(kind) = input.kind {
            phase.kind = kind.trim().to_string();
        }
        if let Some(position) = input.position {
            phase.position = position;
        }
        if let Some(status) = input.status {
            phase.status = status.trim().to_string();
        }
        if let Some(gate_status) = input.gate_status {
            phase.gate_status = gate_status.trim().to_string();
        }
        phase.start_date = start_date;
        phase.end_date = end_date;
        phase.updated_at = now();
        let updated = phase.clone();
        drop(phase);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "phase.updated",
            "phase",
            Some(updated.id.clone()),
            json!({ "name": updated.name, "status": updated.status }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_project_phase(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, phase_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let project = s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
        )?;
        let phase = s4_get_phase(&state, &project_id, &phase_id)?;
        let has_tasks = state
            .tasks
            .iter()
            .any(|entry| entry.value().phase_id.as_deref() == Some(phase_id.as_str()));
        if has_tasks {
            return Err(UnifiedError::bad_request(
                "Cannot delete a phase that has tasks",
            ));
        }
        state
            .phases
            .remove(phase_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Phase not found"))?;
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "phase.deleted",
            "phase",
            Some(phase.id.clone()),
            json!({ "name": phase.name }),
        );
        Ok(json!({ "deleted": true, "id": phase_id }))
    })();
    json::respond(result, false)
}

async fn decide_phase_gate(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<DecidePhaseGate>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, phase_id) = path.into_inner();
    let result: UnifiedResult<ProjectPhase> = (|| {
        let project = s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
        )?;
        let input = body.into_inner();
        let decision = input.decision.trim().to_string();
        if decision != "approved" && decision != "rejected" {
            return Err(UnifiedError::bad_request(
                "Gate decision must be one of: approved, rejected",
            ));
        }
        let note = s4_clean_text(input.note);
        let phase = s4_get_phase(&state, &project_id, &phase_id)?;
        let was_completed = phase.status == "completed";
        let position = phase.position;
        let mut phase = state
            .phases
            .get_mut(phase_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Phase not found"))?;
        phase.gate_status = decision.clone();
        phase.gate_decision = note.clone();
        phase.updated_at = now();
        let decided = phase.clone();
        drop(phase);
        // An approved gate on a completed phase opens the next phase.
        if decision == "approved" && was_completed {
            if let Some(next_id) = s4_next_phase_id(&state, &project.id, position) {
                if let Some(mut next) = state.phases.get_mut(next_id.as_str()) {
                    if next.status == "planned" {
                        next.status = "active".to_string();
                        next.updated_at = now();
                    }
                }
            }
        }
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "phase.gate_decided",
            "phase",
            Some(decided.id.clone()),
            json!({ "name": decided.name, "decision": decision, "note": note }),
        );
        state.emit(
            "project.phase.gate_decided",
            json!({
                "project_id": project.id,
                "phase_id": decided.id,
                "name": decided.name,
                "decision": decision,
                "note": note,
            }),
        );
        state.notify(
            &project.owner_id,
            format!("Phase gate {}: {}", decision, decided.name),
            format!(
                "The gate for phase {} was {}.",
                decided.name, decision
            ),
        );
        Ok(decided)
    })();
    json::respond(result, false)
}

async fn complete_project_phase(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, phase_id) = path.into_inner();
    let result: UnifiedResult<ProjectPhase> = (|| {
        let project = s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
        )?;
        let phase = s4_get_phase(&state, &project_id, &phase_id)?;
        if phase.status == "completed" {
            return Err(UnifiedError::bad_request("Phase is already completed"));
        }
        if phase.kind == "prince2_stage" && phase.gate_status != "approved" {
            return Err(UnifiedError::bad_request(
                "A PRINCE2 stage can only be completed once its gate is approved",
            ));
        }
        let current = now();
        let mut phase = state
            .phases
            .get_mut(phase_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Phase not found"))?;
        phase.status = "completed".to_string();
        if phase.end_date.is_none() {
            phase.end_date = Some(current);
        }
        phase.updated_at = current;
        let completed = phase.clone();
        drop(phase);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "phase.completed",
            "phase",
            Some(completed.id.clone()),
            json!({ "name": completed.name, "kind": completed.kind }),
        );
        state.notify(
            &project.owner_id,
            format!("Phase completed: {}", completed.name),
            format!("Phase {} is now complete.", completed.name),
        );
        Ok(completed)
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Shape Up cycles
// ---------------------------------------------------------------------------

async fn list_project_cycles(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<PageResult<ProjectCycle>> = (|| {
        s4_resolve_project(
            &state,
            path.as_str(),
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:view",
        )?;
        let sort = params.allowed_sort(&["created_at", "name", "updated_at"], "created_at");
        let mut items: Vec<ProjectCycle> = state
            .cycles
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|cycle| {
                cycle.project_id == path.as_str()
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| cycle.status == *status)
                    && matches_search(
                        &format!(
                            "{} {}",
                            cycle.name,
                            cycle.pitch.as_deref().unwrap_or_default()
                        ),
                        &params,
                    )
            })
            .collect();
        match sort.as_str() {
            "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
            "updated_at" => items.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn create_project_cycle(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateProjectCycle>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectCycle> = (|| {
        let project = s4_resolve_project(
            &state,
            path.as_str(),
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
        )?;
        let input = body.into_inner();
        let name = s4_require_text(&input.name, "Cycle name")?;
        let profile = methodology_profile(&project.methodology);
        if !profile.is_some_and(|profile| profile.uses_cycles) {
            return Err(UnifiedError::bad_request(
                "methodology does not use cycles",
            ));
        }
        let appetite_weeks = input.appetite_weeks.unwrap_or(6);
        if !(1..=12).contains(&appetite_weeks) {
            return Err(UnifiedError::bad_request(
                "appetite_weeks must be between 1 and 12",
            ));
        }
        let cooldown_weeks = input.cooldown_weeks.unwrap_or(2);
        let status = input
            .status
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "shaping".to_string());
        if !s4_valid_cycle_status(&status) {
            return Err(UnifiedError::bad_request(
                "Cycle status must be one of: shaping, betting, building, cooldown, done",
            ));
        }
        let current = now();
        let cycle = ProjectCycle {
            id: new_id(),
            project_id: project.id.clone(),
            name,
            appetite_weeks,
            status,
            started_at: input.started_at,
            ends_at: input.ends_at,
            cooldown_weeks,
            pitch: s4_clean_text(input.pitch),
            betting_decision: None,
            created_at: current,
            updated_at: current,
        };
        state.cycles.insert(cycle.id.clone(), cycle.clone());
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "cycle.created",
            "cycle",
            Some(cycle.id.clone()),
            json!({ "name": cycle.name, "appetite_weeks": cycle.appetite_weeks }),
        );
        Ok(cycle)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn get_project_cycle(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, cycle_id) = path.into_inner();
    let result: UnifiedResult<ProjectCycle> = (|| {
        s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:view",
        )?;
        s4_get_cycle(&state, &project_id, &cycle_id)
    })();
    json::respond(result, false)
}

async fn update_project_cycle(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateProjectCycle>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, cycle_id) = path.into_inner();
    let result: UnifiedResult<ProjectCycle> = (|| {
        let project = s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
        )?;
        let input = body.into_inner();
        if let Some(appetite_weeks) = input.appetite_weeks {
            if !(1..=12).contains(&appetite_weeks) {
                return Err(UnifiedError::bad_request(
                    "appetite_weeks must be between 1 and 12",
                ));
            }
        }
        if let Some(status) = &input.status {
            if !s4_valid_cycle_status(status.trim()) {
                return Err(UnifiedError::bad_request(
                    "Cycle status must be one of: shaping, betting, building, cooldown, done",
                ));
            }
        }
        let mut cycle = state
            .cycles
            .get_mut(cycle_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Cycle not found"))?;
        if cycle.project_id != project_id {
            return Err(UnifiedError::not_found("Cycle not found"));
        }
        if let Some(name) = input.name {
            cycle.name = s4_require_text(&name, "Cycle name")?;
        }
        if let Some(appetite_weeks) = input.appetite_weeks {
            cycle.appetite_weeks = appetite_weeks;
        }
        if let Some(cooldown_weeks) = input.cooldown_weeks {
            cycle.cooldown_weeks = cooldown_weeks;
        }
        if let Some(status) = input.status {
            cycle.status = status.trim().to_string();
        }
        if let Some(pitch) = input.pitch {
            cycle.pitch = s4_clean_text(Some(pitch));
        }
        if let Some(started_at) = input.started_at {
            cycle.started_at = Some(started_at);
        }
        if let Some(ends_at) = input.ends_at {
            cycle.ends_at = Some(ends_at);
        }
        cycle.updated_at = now();
        let updated = cycle.clone();
        drop(cycle);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "cycle.updated",
            "cycle",
            Some(updated.id.clone()),
            json!({ "name": updated.name, "status": updated.status }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_project_cycle(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, cycle_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let project = s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
        )?;
        let cycle = s4_get_cycle(&state, &project_id, &cycle_id)?;
        state
            .cycles
            .remove(cycle_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Cycle not found"))?;
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "cycle.deleted",
            "cycle",
            Some(cycle.id.clone()),
            json!({ "name": cycle.name }),
        );
        Ok(json!({ "deleted": true, "id": cycle_id }))
    })();
    json::respond(result, false)
}

async fn bet_project_cycle(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<BetProjectCycle>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, cycle_id) = path.into_inner();
    let result: UnifiedResult<ProjectCycle> = (|| {
        let project = s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
        )?;
        let input = body.into_inner();
        let decision = input.decision.trim().to_string();
        if decision != "bet" && decision != "pass" {
            return Err(UnifiedError::bad_request(
                "Bet decision must be one of: bet, pass",
            ));
        }
        let pitch = s4_clean_text(input.pitch);
        let cycle = s4_get_cycle(&state, &project_id, &cycle_id)?;
        if cycle.status == "done" {
            return Err(UnifiedError::bad_request(
                "Cannot bet on a cycle that is already done",
            ));
        }
        let mut cycle = state
            .cycles
            .get_mut(cycle_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Cycle not found"))?;
        if decision == "bet" {
            if cycle.status != "betting" {
                return Err(UnifiedError::bad_request(
                    "Only a cycle in betting can be bet on",
                ));
            }
            cycle.status = "building".to_string();
            cycle.betting_decision = Some("bet".to_string());
        } else {
            cycle.status = "done".to_string();
            cycle.betting_decision = Some(pitch.clone().unwrap_or_else(|| "pass".to_string()));
        }
        if let Some(pitch) = pitch {
            cycle.pitch = Some(pitch);
        }
        cycle.updated_at = now();
        let updated = cycle.clone();
        drop(cycle);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "cycle.bet",
            "cycle",
            Some(updated.id.clone()),
            json!({ "name": updated.name, "decision": decision, "status": updated.status }),
        );
        state.notify(
            &project.owner_id,
            format!("Cycle {}: {}", decision, updated.name),
            format!(
                "The betting decision for cycle {} is {}.",
                updated.name, decision
            ),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn complete_project_cycle(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, cycle_id) = path.into_inner();
    let result: UnifiedResult<ProjectCycle> = (|| {
        let project = s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
        )?;
        let cycle = s4_get_cycle(&state, &project_id, &cycle_id)?;
        if cycle.status == "done" {
            return Err(UnifiedError::bad_request("Cycle is already done"));
        }
        let current = now();
        let mut cycle = state
            .cycles
            .get_mut(cycle_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Cycle not found"))?;
        cycle.status = "done".to_string();
        if cycle.ends_at.is_none() {
            cycle.ends_at = Some(current);
        }
        cycle.updated_at = current;
        let completed = cycle.clone();
        drop(cycle);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "cycle.completed",
            "cycle",
            Some(completed.id.clone()),
            json!({ "name": completed.name }),
        );
        state.notify(
            &project.owner_id,
            format!("Cycle done: {}", completed.name),
            format!("Cycle {} is now done.", completed.name),
        );
        Ok(completed)
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// SAFe program increments
// ---------------------------------------------------------------------------

async fn list_program_increments(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<PageResult<ProjectProgramIncrement>> = (|| {
        s4_resolve_project(
            &state,
            path.as_str(),
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:view",
        )?;
        let sort = params.allowed_sort(
            &["created_at", "name", "start_date", "end_date", "updated_at"],
            "created_at",
        );
        let mut items: Vec<ProjectProgramIncrement> = state
            .program_increments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|pi| {
                pi.project_id == path.as_str()
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| pi.status == *status)
                    && matches_search(&pi.name, &params)
            })
            .collect();
        match sort.as_str() {
            "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
            "start_date" => items.sort_by(|a, b| a.start_date.cmp(&b.start_date)),
            "end_date" => items.sort_by(|a, b| a.end_date.cmp(&b.end_date)),
            "updated_at" => items.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn create_program_increment(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateProgramIncrement>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectProgramIncrement> = (|| {
        let project = s4_resolve_project(
            &state,
            path.as_str(),
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
        )?;
        let input = body.into_inner();
        let name = s4_require_text(&input.name, "Program increment name")?;
        let profile = methodology_profile(&project.methodology);
        if !profile.is_some_and(|profile| profile.uses_program_increments) {
            return Err(UnifiedError::bad_request(
                "methodology does not use program increments",
            ));
        }
        let status = input
            .status
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "planned".to_string());
        if !s4_valid_pi_status(&status) {
            return Err(UnifiedError::bad_request(
                "Program increment status must be one of: planned, active, completed",
            ));
        }
        s4_validate_date_range(input.start_date, input.end_date, "Program increment")?;
        let current = now();
        let pi = ProjectProgramIncrement {
            id: new_id(),
            project_id: project.id.clone(),
            name,
            status,
            start_date: input.start_date,
            end_date: input.end_date,
            objectives: input.objectives.unwrap_or_else(|| Value::Array(Vec::new())),
            created_at: current,
            updated_at: current,
        };
        state
            .program_increments
            .insert(pi.id.clone(), pi.clone());
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "program_increment.created",
            "program_increment",
            Some(pi.id.clone()),
            json!({ "name": pi.name, "status": pi.status }),
        );
        Ok(pi)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn get_program_increment(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, pi_id) = path.into_inner();
    let result: UnifiedResult<ProjectProgramIncrement> = (|| {
        s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:view",
        )?;
        s4_get_program_increment(&state, &project_id, &pi_id)
    })();
    json::respond(result, false)
}

async fn update_program_increment(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateProgramIncrement>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, pi_id) = path.into_inner();
    let result: UnifiedResult<ProjectProgramIncrement> = (|| {
        let project = s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
        )?;
        let input = body.into_inner();
        if let Some(status) = &input.status {
            if !s4_valid_pi_status(status.trim()) {
                return Err(UnifiedError::bad_request(
                    "Program increment status must be one of: planned, active, completed",
                ));
            }
        }
        let mut pi = state
            .program_increments
            .get_mut(pi_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Program increment not found"))?;
        if pi.project_id != project_id {
            return Err(UnifiedError::not_found("Program increment not found"));
        }
        let start_date = input.start_date.or(pi.start_date);
        let end_date = input.end_date.or(pi.end_date);
        s4_validate_date_range(start_date, end_date, "Program increment")?;
        if let Some(name) = input.name {
            pi.name = s4_require_text(&name, "Program increment name")?;
        }
        if let Some(status) = input.status {
            pi.status = status.trim().to_string();
        }
        pi.start_date = start_date;
        pi.end_date = end_date;
        if let Some(objectives) = input.objectives {
            pi.objectives = objectives;
        }
        pi.updated_at = now();
        let updated = pi.clone();
        drop(pi);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "program_increment.updated",
            "program_increment",
            Some(updated.id.clone()),
            json!({ "name": updated.name, "status": updated.status }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_program_increment(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, pi_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let project = s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
        )?;
        let pi = s4_get_program_increment(&state, &project_id, &pi_id)?;
        state
            .program_increments
            .remove(pi_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Program increment not found"))?;
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "program_increment.deleted",
            "program_increment",
            Some(pi.id.clone()),
            json!({ "name": pi.name }),
        );
        Ok(json!({ "deleted": true, "id": pi_id }))
    })();
    json::respond(result, false)
}

async fn plan_program_increment(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<PlanProgramIncrement>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, pi_id) = path.into_inner();
    let result: UnifiedResult<ProjectProgramIncrement> = (|| {
        let project = s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:manage",
        )?;
        let input = body.into_inner();
        let mut pi = state
            .program_increments
            .get_mut(pi_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Program increment not found"))?;
        if pi.project_id != project_id {
            return Err(UnifiedError::not_found("Program increment not found"));
        }
        // Planning records the plan and keeps the increment planned;
        // it does not start the increment.
        pi.objectives = Value::Array(input.objectives);
        pi.status = "planned".to_string();
        pi.updated_at = now();
        let planned = pi.clone();
        drop(pi);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "program_increment.planned",
            "program_increment",
            Some(planned.id.clone()),
            json!({ "name": planned.name, "objectives": planned.objectives }),
        );
        state.notify(
            &project.owner_id,
            format!("Program increment planned: {}", planned.name),
            format!(
                "The plan for program increment {} was recorded.",
                planned.name
            ),
        );
        Ok(planned)
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// PRINCE2 business cases
// ---------------------------------------------------------------------------

async fn list_business_cases(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<PageResult<ProjectBusinessCase>> = (|| {
        s4_resolve_project(
            &state,
            path.as_str(),
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:view",
        )?;
        let sort = params.allowed_sort(&["created_at", "title", "updated_at"], "created_at");
        let mut items: Vec<ProjectBusinessCase> = state
            .business_cases
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|case| {
                case.project_id == path.as_str()
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| case.status == *status)
                    && matches_search(
                        &format!(
                            "{} {}",
                            case.title,
                            case.justification.as_deref().unwrap_or_default()
                        ),
                        &params,
                    )
            })
            .collect();
        match sort.as_str() {
            "title" => items.sort_by(|a, b| a.title.cmp(&b.title)),
            "updated_at" => items.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn create_business_case(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateBusinessCase>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectBusinessCase> = (|| {
        let project = s4_resolve_project(
            &state,
            path.as_str(),
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:manage",
        )?;
        let input = body.into_inner();
        let title = s4_require_text(&input.title, "Business case title")?;
        let status = input
            .status
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "draft".to_string());
        if !s4_valid_business_case_status(&status) {
            return Err(UnifiedError::bad_request(
                "Business case status must be one of: draft, approved, rejected",
            ));
        }
        let current = now();
        let case = ProjectBusinessCase {
            id: new_id(),
            project_id: project.id.clone(),
            title,
            justification: s4_clean_text(input.justification),
            benefits: input.benefits.unwrap_or_else(|| Value::Array(Vec::new())),
            costs: input.costs.unwrap_or_else(|| Value::Array(Vec::new())),
            risks: input.risks.unwrap_or_else(|| Value::Array(Vec::new())),
            status,
            exception_report: None,
            created_at: current,
            updated_at: current,
        };
        state
            .business_cases
            .insert(case.id.clone(), case.clone());
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "business_case.created",
            "business_case",
            Some(case.id.clone()),
            json!({ "title": case.title, "status": case.status }),
        );
        Ok(case)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn get_business_case(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, case_id) = path.into_inner();
    let result: UnifiedResult<ProjectBusinessCase> = (|| {
        s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:view",
        )?;
        s4_get_business_case(&state, &project_id, &case_id)
    })();
    json::respond(result, false)
}

async fn update_business_case(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateBusinessCase>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, case_id) = path.into_inner();
    let result: UnifiedResult<ProjectBusinessCase> = (|| {
        let project = s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:manage",
        )?;
        let input = body.into_inner();
        if let Some(status) = &input.status {
            if !s4_valid_business_case_status(status.trim()) {
                return Err(UnifiedError::bad_request(
                    "Business case status must be one of: draft, approved, rejected",
                ));
            }
        }
        let mut case = state
            .business_cases
            .get_mut(case_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Business case not found"))?;
        if case.project_id != project_id {
            return Err(UnifiedError::not_found("Business case not found"));
        }
        if let Some(title) = input.title {
            case.title = s4_require_text(&title, "Business case title")?;
        }
        if let Some(justification) = input.justification {
            case.justification = s4_clean_text(Some(justification));
        }
        if let Some(benefits) = input.benefits {
            case.benefits = benefits;
        }
        if let Some(costs) = input.costs {
            case.costs = costs;
        }
        if let Some(risks) = input.risks {
            case.risks = risks;
        }
        if let Some(status) = input.status {
            case.status = status.trim().to_string();
        }
        case.updated_at = now();
        let updated = case.clone();
        drop(case);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "business_case.updated",
            "business_case",
            Some(updated.id.clone()),
            json!({ "title": updated.title, "status": updated.status }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_business_case(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, case_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let project = s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:manage",
        )?;
        let case = s4_get_business_case(&state, &project_id, &case_id)?;
        state
            .business_cases
            .remove(case_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Business case not found"))?;
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "business_case.deleted",
            "business_case",
            Some(case.id.clone()),
            json!({ "title": case.title }),
        );
        Ok(json!({ "deleted": true, "id": case_id }))
    })();
    json::respond(result, false)
}

async fn save_exception_report(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<SaveExceptionReport>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let (project_id, case_id) = path.into_inner();
    let result: UnifiedResult<ProjectBusinessCase> = (|| {
        let project = s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:manage",
        )?;
        let input = body.into_inner();
        let report = s4_require_text(&input.report, "Exception report")?;
        let mut case = state
            .business_cases
            .get_mut(case_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Business case not found"))?;
        if case.project_id != project_id {
            return Err(UnifiedError::not_found("Business case not found"));
        }
        case.exception_report = Some(report);
        case.updated_at = now();
        let updated = case.clone();
        drop(case);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "business_case.exception_report_saved",
            "business_case",
            Some(updated.id.clone()),
            json!({ "title": updated.title }),
        );
        state.notify(
            &project.owner_id,
            format!("Exception report saved: {}", updated.title),
            format!(
                "An exception report was saved for business case {}.",
                updated.title
            ),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}
// ---------------------------------------------------------------------------
// Projects module - section S5.
//
// Time tracking (entries, timers, billable hours), workload, views
// (Gantt, timeline, calendar), reports (burndown, velocity, cumulative
// flow, cycle time, critical path), project import/export, and the
// project activity feed.
//
// This section only defines the handlers listed here plus the input
// structs and private helpers it needs. Imports, entity structs, the
// state type, shared helpers, and the module wiring live in section S1.
// Every private helper in this section is prefixed with `s5_` so it
// cannot collide with helpers from other sections.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Input structs
// ---------------------------------------------------------------------------

/// Body for creating a time entry.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateTimeEntry {
    pub task_id: String,
    pub user_id: String,
    #[serde(default)]
    pub description: Option<String>,
    pub hours: f64,
    #[serde(default)]
    pub billable: Option<bool>,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub ended_at: Option<DateTime<Utc>>,
}

/// Body for updating a time entry. Every field is optional; only the
/// fields present are changed.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTimeEntry {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub hours: Option<f64>,
    #[serde(default)]
    pub billable: Option<bool>,
    #[serde(default)]
    pub billed: Option<bool>,
    #[serde(default)]
    pub invoice_id: Option<String>,
}

/// Body for starting a running time timer on a task.
#[derive(Debug, Clone, Deserialize)]
pub struct StartTimer {
    pub task_id: String,
    pub user_id: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// One task row inside an import payload.
#[derive(Debug, Clone, Deserialize)]
pub struct ImportTaskRow {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub task_type: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub assignee_id: Option<String>,
    #[serde(default)]
    pub estimate_points: Option<f64>,
    #[serde(default)]
    pub estimate_hours: Option<f64>,
    #[serde(default)]
    pub due_date: Option<DateTime<Utc>>,
}

/// Body for importing tasks into a project. Only JSON imports are
/// supported; rows that fail validation are collected into `errors`
/// and skipped instead of failing the whole import.
#[derive(Debug, Clone, Deserialize)]
pub struct ImportProject {
    pub format: String,
    #[serde(default)]
    pub tasks: Vec<ImportTaskRow>,
}

// ---------------------------------------------------------------------------
// Private helpers (all prefixed `s5_`)
// ---------------------------------------------------------------------------

/// Round a float to two decimal places.
fn s5_round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// Round a float to four decimal places.
fn s5_round4(value: f64) -> f64 {
    (value * 10000.0).round() / 10000.0
}

/// A task counts as done when it has a completion timestamp or its
/// status is one of the terminal done spellings.
fn s5_task_is_done(task: &ProjectTask) -> bool {
    task.completed_at.is_some() || task.status == "done" || task.status == "completed"
}

/// A task counts as open when it is neither done nor cancelled/closed.
fn s5_task_is_open(task: &ProjectTask) -> bool {
    !s5_task_is_done(task)
        && task.status != "cancelled"
        && task.status != "closed"
        && task.status != "archived"
}

/// Hours for a single time entry must be greater than zero and at most
/// one full day.
fn s5_valid_hours(hours: f64) -> bool {
    hours > 0.0 && hours <= 24.0
}

/// Parse a `"true"` / `"false"` query filter value.
fn s5_parse_flag(value: Option<&String>) -> Option<bool> {
    match value.map(String::as_str) {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    }
}

/// Escape one CSV field: double any double quotes, and wrap the field
/// in double quotes when it contains a comma, a quote, or a newline.
fn s5_csv_escape(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r')
    {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

/// Format an optional float for CSV output: empty when absent.
fn s5_csv_number(value: Option<f64>) -> String {
    match value {
        Some(number) => {
            if number == number.trunc() {
                format!("{}", number as i64)
            } else {
                format!("{number}")
            }
        }
        None => String::new(),
    }
}

/// Format an optional timestamp for CSV output (RFC 3339, empty when
/// absent).
fn s5_csv_datetime(value: Option<DateTime<Utc>>) -> String {
    match value {
        Some(moment) => moment.to_rfc3339(),
        None => String::new(),
    }
}

/// Median of a sorted slice of floats. Returns 0.0 for an empty slice.
fn s5_median(sorted: &[f64]) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[middle]
    } else {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    }
}

/// Nearest-rank percentile of a sorted slice. Returns 0.0 for an empty
/// slice. `percentile` is 0.0..=100.0.
fn s5_percentile(sorted: &[f64], percentile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (percentile / 100.0 * sorted.len() as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[index]
}

/// End of a calendar day (23:59:59 UTC).
fn s5_end_of_day(day: chrono::NaiveDate) -> DateTime<Utc> {
    day.and_hms_opt(23, 59, 59)
        .map(|naive| naive.and_utc())
        .unwrap_or_else(now)
}

/// Recursive depth-first search used to recover one concrete cycle
/// from a dependency graph that is not a DAG. `state` maps a task id
/// to 1 while it is on the current path and 2 once fully explored.
/// Returns the ids forming the cycle, with the closing id repeated
/// at the end, or `None` when no cycle is reachable from `node`.
fn s5_cycle_dfs(
    node: &str,
    successors: &std::collections::HashMap<String, Vec<String>>,
    state: &mut std::collections::HashMap<String, u8>,
    stack: &mut Vec<String>,
) -> Option<Vec<String>> {
    state.insert(node.to_string(), 1);
    stack.push(node.to_string());
    if let Some(next_nodes) = successors.get(node) {
        for next in next_nodes {
            let next_state = state.get(next).copied().unwrap_or(0);
            if next_state == 1 {
                let position = stack.iter().position(|id| id == next).unwrap_or(0);
                let mut cycle: Vec<String> = stack[position..].to_vec();
                cycle.push(next.clone());
                return Some(cycle);
            }
            if next_state == 0 {
                if let Some(cycle) = s5_cycle_dfs(next, successors, state, stack) {
                    return Some(cycle);
                }
            }
        }
    }
    stack.pop();
    state.insert(node.to_string(), 2);
    None
}

/// Turn a create result into a 201 success response or an error
/// response, using the standard envelope.
fn s5_respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

/// Emit the time-logged events for a time entry: always
/// `project.time.logged`, and additionally `billing.time_logged` when
/// the entry is billable so billing can invoice it. Emission only;
/// this never calls billing directly.
fn s5_emit_time_logged(state: &ProjectsState, entry: &ProjectTimeEntry) {
    state.emit(
        "project.time.logged",
        json!({
            "project_id": entry.project_id,
            "task_id": entry.task_id,
            "entry_id": entry.id,
            "user_id": entry.user_id,
            "hours": entry.hours,
            "billable": entry.billable,
        }),
    );
    if entry.billable {
        state.emit(
            "billing.time_logged",
            json!({
                "project_id": entry.project_id,
                "task_id": entry.task_id,
                "user_id": entry.user_id,
                "hours": entry.hours,
                "entry_id": entry.id,
            }),
        );
    }
}

// ---------------------------------------------------------------------------
// Time tracking
// ---------------------------------------------------------------------------

async fn list_time_entries(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result = (|| {
        let project = state.get_project(path.as_str())?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        require_capability(&state, viewer_role.as_deref(), "projects:view")?;
        let sort = params.allowed_sort(&["started_at", "hours", "created_at"], "started_at");
        let mut items: Vec<ProjectTimeEntry> = state
            .time_entries
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                entry.project_id == project.id
                    && params
                        .filters
                        .get("user_id")
                        .is_none_or(|value| entry.user_id == *value)
                    && params
                        .filters
                        .get("task_id")
                        .is_none_or(|value| entry.task_id == *value)
                    && params.filters.get("billable").is_none_or(|value| {
                        s5_parse_flag(Some(value)) == Some(entry.billable)
                    })
                    && params.filters.get("billed").is_none_or(|value| {
                        s5_parse_flag(Some(value)) == Some(entry.billed)
                    })
                    && matches_search(
                        &format!("{} {}", entry.description, entry.user_id),
                        &params,
                    )
            })
            .collect();
        match sort.as_str() {
            "hours" => items.sort_by(|a, b| {
                a.hours
                    .partial_cmp(&b.hours)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            "created_at" => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
            _ => items.sort_by(|a, b| a.started_at.cmp(&b.started_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn create_time_entry(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateTimeEntry>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectTimeEntry> = (|| {
        let project = state.get_project(path.as_str())?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        require_capability(&state, viewer_role.as_deref(), "projects:time_log")?;
        let input = body.into_inner();
        let task = state.get_task(&input.task_id)?;
        if task.project_id != project.id {
            return Err(UnifiedError::bad_request(
                "Task does not belong to this project",
            ));
        }
        if !s5_valid_hours(input.hours) {
            return Err(UnifiedError::bad_request(
                "Hours must be greater than 0 and at most 24",
            ));
        }
        let started_at = input.started_at.unwrap_or_else(now);
        if let Some(ended_at) = input.ended_at {
            if ended_at <= started_at {
                return Err(UnifiedError::bad_request(
                    "ended_at must be after started_at",
                ));
            }
        }
        let user_id = input.user_id.trim().to_string();
        if user_id.is_empty() {
            return Err(UnifiedError::bad_request("user_id must not be empty"));
        }
        let entry = ProjectTimeEntry {
            id: new_id(),
            task_id: task.id.clone(),
            project_id: project.id.clone(),
            user_id: user_id.clone(),
            description: input
                .description
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_default(),
            hours: s5_round2(input.hours),
            billable: input.billable.unwrap_or(false),
            billed: false,
            invoice_id: None,
            started_at,
            ended_at: input.ended_at,
            timer_running: false,
            created_at: now(),
            updated_at: now(),
        };
        state
            .time_entries
            .insert(entry.id.clone(), entry.clone());
        state.log_activity(
            &project.id,
            viewer_id.as_deref().unwrap_or(&user_id),
            "time_entry.created",
            "time_entry",
            Some(entry.id.clone()),
            json!({
                "task_id": entry.task_id,
                "hours": entry.hours,
                "billable": entry.billable,
            }),
        );
        s5_emit_time_logged(&state, &entry);
        Ok(entry)
    })();
    s5_respond_created(result)
}

async fn get_time_entry(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectTimeEntry> = (|| {
        let (project_id, entry_id) = path.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        require_capability(&state, viewer_role.as_deref(), "projects:view")?;
        let entry = state
            .time_entries
            .get(&entry_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Time entry not found"))?;
        if entry.project_id != project.id {
            return Err(UnifiedError::not_found("Time entry not found"));
        }
        Ok(entry)
    })();
    json::respond(result, false)
}

async fn update_time_entry(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateTimeEntry>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectTimeEntry> = (|| {
        let (project_id, entry_id) = path.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        require_capability(&state, viewer_role.as_deref(), "projects:time_log")?;
        let input = body.into_inner();
        if let Some(hours) = input.hours {
            if !s5_valid_hours(hours) {
                return Err(UnifiedError::bad_request(
                    "Hours must be greater than 0 and at most 24",
                ));
            }
        }
        let mut entry = state
            .time_entries
            .get_mut(&entry_id)
            .ok_or_else(|| UnifiedError::not_found("Time entry not found"))?;
        if entry.project_id != project.id {
            return Err(UnifiedError::not_found("Time entry not found"));
        }
        if entry.billed {
            return Err(UnifiedError::bad_request(
                "A billed time entry cannot be edited",
            ));
        }
        if let Some(description) = input.description {
            entry.description = description.trim().to_string();
        }
        if let Some(hours) = input.hours {
            entry.hours = s5_round2(hours);
        }
        if let Some(billable) = input.billable {
            entry.billable = billable;
        }
        if let Some(billed) = input.billed {
            entry.billed = billed;
        }
        if let Some(invoice_id) = input.invoice_id {
            entry.invoice_id = Some(invoice_id.trim().to_string());
        }
        entry.updated_at = now();
        let updated = entry.clone();
        let actor = viewer_id.clone().unwrap_or_else(|| updated.user_id.clone());
        drop(entry);
        state.log_activity(
            &project.id,
            &actor,
            "time_entry.updated",
            "time_entry",
            Some(updated.id.clone()),
            json!({
                "task_id": updated.task_id,
                "hours": updated.hours,
            }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_time_entry(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Value> = (|| {
        let (project_id, entry_id) = path.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        require_capability(&state, viewer_role.as_deref(), "projects:time_log")?;
        let entry = state
            .time_entries
            .get(&entry_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Time entry not found"))?;
        if entry.project_id != project.id {
            return Err(UnifiedError::not_found("Time entry not found"));
        }
        if entry.billed {
            return Err(UnifiedError::bad_request(
                "A billed time entry cannot be deleted",
            ));
        }
        state.time_entries.remove(&entry_id);
        state.log_activity(
            &project.id,
            viewer_id.as_deref().unwrap_or(&entry.user_id),
            "time_entry.deleted",
            "time_entry",
            Some(entry_id.clone()),
            json!({
                "task_id": entry.task_id,
                "hours": entry.hours,
            }),
        );
        Ok(json!({ "deleted": true, "id": entry_id }))
    })();
    json::respond(result, false)
}

async fn start_time_timer(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<StartTimer>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectTimeEntry> = (|| {
        let project = state.get_project(path.as_str())?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        require_capability(&state, viewer_role.as_deref(), "projects:time_log")?;
        let input = body.into_inner();
        let task = state.get_task(&input.task_id)?;
        if task.project_id != project.id {
            return Err(UnifiedError::bad_request(
                "Task does not belong to this project",
            ));
        }
        let user_id = input.user_id.trim().to_string();
        if user_id.is_empty() {
            return Err(UnifiedError::bad_request("user_id must not be empty"));
        }
        let already_running = state.time_entries.iter().any(|entry| {
            let entry = entry.value();
            entry.project_id == project.id
                && entry.user_id == user_id
                && entry.timer_running
        });
        if already_running {
            return Err(UnifiedError::bad_request(
                "User already has a running timer in this project",
            ));
        }
        let entry = ProjectTimeEntry {
            id: new_id(),
            task_id: task.id.clone(),
            project_id: project.id.clone(),
            user_id: user_id.clone(),
            description: input
                .description
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_default(),
            hours: 0.0,
            billable: false,
            billed: false,
            invoice_id: None,
            started_at: now(),
            ended_at: None,
            timer_running: true,
            created_at: now(),
            updated_at: now(),
        };
        state
            .time_entries
            .insert(entry.id.clone(), entry.clone());
        state.log_activity(
            &project.id,
            viewer_id.as_deref().unwrap_or(&user_id),
            "timer.started",
            "time_entry",
            Some(entry.id.clone()),
            json!({ "task_id": entry.task_id }),
        );
        Ok(entry)
    })();
    s5_respond_created(result)
}

async fn stop_time_timer(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<ProjectTimeEntry> = (|| {
        let (project_id, entry_id) = path.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        require_capability(&state, viewer_role.as_deref(), "projects:time_log")?;
        let mut entry = state
            .time_entries
            .get_mut(&entry_id)
            .ok_or_else(|| UnifiedError::not_found("Time entry not found"))?;
        if entry.project_id != project.id {
            return Err(UnifiedError::not_found("Time entry not found"));
        }
        if !entry.timer_running {
            return Err(UnifiedError::bad_request("Timer is not running"));
        }
        let stopped_at = now();
        let elapsed_ms = stopped_at
            .signed_duration_since(entry.started_at)
            .num_milliseconds();
        let hours = s5_round2(elapsed_ms as f64 / 3_600_000.0).max(0.01);
        entry.timer_running = false;
        entry.ended_at = Some(stopped_at);
        entry.hours = hours;
        entry.updated_at = stopped_at;
        let stopped = entry.clone();
        let actor = viewer_id.clone().unwrap_or_else(|| stopped.user_id.clone());
        drop(entry);
        state.log_activity(
            &project.id,
            &actor,
            "timer.stopped",
            "time_entry",
            Some(stopped.id.clone()),
            json!({
                "task_id": stopped.task_id,
                "hours": stopped.hours,
            }),
        );
        s5_emit_time_logged(&state, &stopped);
        Ok(stopped)
    })();
    json::respond(result, false)
}

async fn list_billable_time(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Value> = (|| {
        let project = state.get_project(path.as_str())?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        require_capability(&state, viewer_role.as_deref(), "projects:reports")?;
        let sort = params.allowed_sort(&["started_at", "hours", "created_at"], "started_at");
        let mut items: Vec<ProjectTimeEntry> = state
            .time_entries
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                entry.project_id == project.id
                    && entry.billable
                    && !entry.billed
                    && params
                        .filters
                        .get("user_id")
                        .is_none_or(|value| entry.user_id == *value)
                    && params
                        .filters
                        .get("task_id")
                        .is_none_or(|value| entry.task_id == *value)
            })
            .collect();
        let total_hours = s5_round2(items.iter().map(|entry| entry.hours).sum::<f64>());
        // Every entry in this list is billable, so the billable total
        // is the same sum; both keys are returned because that is the
        // shape the billing module consumes.
        let total_billable_hours = total_hours;
        match sort.as_str() {
            "hours" => items.sort_by(|a, b| {
                a.hours
                    .partial_cmp(&b.hours)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            "created_at" => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
            _ => items.sort_by(|a, b| a.started_at.cmp(&b.started_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        let page = paginate(items, &params);
        Ok(json!({
            "entries": page,
            "total_hours": total_hours,
            "total_billable_hours": total_billable_hours,
        }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Workload
// ---------------------------------------------------------------------------

async fn get_project_workload(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Value> = (|| {
        let project = state.get_project(path.as_str())?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        require_capability(&state, viewer_role.as_deref(), "projects:view")?;
        let capacity_hours = params
            .filters
            .get("capacity_hours")
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| *value > 0.0)
            .unwrap_or(40.0);
        // Rolling seven-day window for "this week" logged hours.
        let week_start = now() - chrono::Duration::days(7);
        let tasks: Vec<ProjectTask> = state
            .tasks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|task| task.project_id == project.id)
            .collect();
        let mut member_ids: Vec<String> = state
            .members
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|member| member.project_id == project.id)
            .map(|member| member.user_id)
            .collect();
        member_ids.sort();
        member_ids.dedup();
        if member_ids.is_empty() {
            // No roster recorded: fall back to the distinct assignees
            // of open tasks so the workload is never empty.
            let mut assignees: Vec<String> = tasks
                .iter()
                .filter(|task| s5_task_is_open(task))
                .filter_map(|task| task.assignee_id.clone())
                .collect();
            assignees.sort();
            assignees.dedup();
            member_ids = assignees;
        }
        let mut workload: Vec<Value> = member_ids
            .iter()
            .map(|user_id| {
                let open_tasks: Vec<&ProjectTask> = tasks
                    .iter()
                    .filter(|task| {
                        task.assignee_id.as_deref() == Some(user_id.as_str())
                            && s5_task_is_open(task)
                    })
                    .collect();
                let assigned_hours = s5_round2(
                    open_tasks
                        .iter()
                        .map(|task| task.estimate_hours.unwrap_or(0.0))
                        .sum::<f64>(),
                );
                let logged_hours_week = s5_round2(
                    state
                        .time_entries
                        .iter()
                        .map(|entry| entry.value().clone())
                        .filter(|entry| {
                            entry.project_id == project.id
                                && entry.user_id == *user_id
                                && entry.started_at >= week_start
                        })
                        .map(|entry| entry.hours)
                        .sum::<f64>(),
                );
                let utilization = if capacity_hours > 0.0 {
                    s5_round4(assigned_hours / capacity_hours)
                } else {
                    0.0
                };
                json!({
                    "user_id": user_id,
                    "open_tasks": open_tasks.len(),
                    "assigned_hours": assigned_hours,
                    "logged_hours_week": logged_hours_week,
                    "capacity_hours": capacity_hours,
                    "utilization": utilization,
                })
            })
            .collect();
        workload.sort_by(|a, b| {
            let left = a.get("utilization").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let right = b.get("utilization").and_then(|v| v.as_f64()).unwrap_or(0.0);
            right
                .partial_cmp(&left)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(json!(workload))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Views: Gantt, timeline, calendar
// ---------------------------------------------------------------------------

async fn get_project_gantt(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Value> = (|| {
        let project = state.get_project(path.as_str())?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        require_capability(&state, viewer_role.as_deref(), "projects:view")?;
        let mut tasks: Vec<ProjectTask> = state
            .tasks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|task| task.project_id == project.id)
            .collect();
        tasks.sort_by(|a, b| {
            let start_a = a.start_date.unwrap_or(a.created_at);
            let start_b = b.start_date.unwrap_or(b.created_at);
            start_a.cmp(&start_b).then(a.title.cmp(&b.title))
        });
        let rows: Vec<Value> = tasks
            .iter()
            .map(|task| {
                let start = task.start_date.unwrap_or(task.created_at);
                let end = task
                    .due_date
                    .unwrap_or_else(|| start + chrono::Duration::days(1));
                let dependencies: Vec<String> = state
                    .dependencies
                    .iter()
                    .map(|entry| entry.value().clone())
                    .filter(|dependency| dependency.task_id == task.id)
                    .map(|dependency| dependency.depends_on_task_id)
                    .collect();
                json!({
                    "task_id": task.id,
                    "title": task.title,
                    "start": start,
                    "end": end,
                    "progress": if s5_task_is_done(task) { 100 } else { 0 },
                    "dependencies": dependencies,
                })
            })
            .collect();
        Ok(json!(rows))
    })();
    json::respond(result, false)
}

async fn get_project_timeline(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Value> = (|| {
        let project = state.get_project(path.as_str())?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        require_capability(&state, viewer_role.as_deref(), "projects:view")?;
        let mut rows: Vec<(DateTime<Utc>, Value)> = Vec::new();
        for milestone in state.milestones.iter().map(|entry| entry.value().clone()) {
            if milestone.project_id != project.id {
                continue;
            }
            if let Some(date) = milestone.due_date {
                rows.push((
                    date,
                    json!({
                        "date": date,
                        "kind": "milestone",
                        "title": milestone.name,
                        "id": milestone.id,
                    }),
                ));
            }
        }
        for sprint in state.sprints.iter().map(|entry| entry.value().clone()) {
            if sprint.project_id != project.id {
                continue;
            }
            if let Some(date) = sprint.start_date.or(sprint.end_date) {
                rows.push((
                    date,
                    json!({
                        "date": date,
                        "kind": "sprint",
                        "title": sprint.name,
                        "id": sprint.id,
                    }),
                ));
            }
        }
        for phase in state.phases.iter().map(|entry| entry.value().clone()) {
            if phase.project_id != project.id {
                continue;
            }
            if let Some(date) = phase.start_date.or(phase.end_date) {
                rows.push((
                    date,
                    json!({
                        "date": date,
                        "kind": "phase",
                        "title": phase.name,
                        "id": phase.id,
                    }),
                ));
            }
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        let items: Vec<Value> = rows.into_iter().map(|(_, row)| row).collect();
        Ok(json!(items))
    })();
    json::respond(result, false)
}

async fn get_project_calendar(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Value> = (|| {
        let project = state.get_project(path.as_str())?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        require_capability(&state, viewer_role.as_deref(), "projects:view")?;
        let current = now();
        let year = match params.filters.get("year") {
            Some(value) => value.parse::<i32>().map_err(|_| {
                UnifiedError::bad_request("year must be a valid year")
            })?,
            None => current.format("%Y").to_string().parse::<i32>().unwrap_or(2026),
        };
        let month = match params.filters.get("month") {
            Some(value) => value.parse::<u32>().map_err(|_| {
                UnifiedError::bad_request("month must be a number between 1 and 12")
            })?,
            None => current.format("%m").to_string().parse::<u32>().unwrap_or(1),
        };
        if !(1970..=2100).contains(&year) {
            return Err(UnifiedError::bad_request(
                "year must be between 1970 and 2100",
            ));
        }
        if !(1..=12).contains(&month) {
            return Err(UnifiedError::bad_request(
                "month must be between 1 and 12",
            ));
        }
        // Group task and milestone due dates by calendar day within
        // the requested month. Days without items are omitted.
        let mut days: std::collections::BTreeMap<String, (Vec<Value>, Vec<Value>)> =
            std::collections::BTreeMap::new();
        for task in state.tasks.iter().map(|entry| entry.value().clone()) {
            if task.project_id != project.id {
                continue;
            }
            if let Some(due) = task.due_date {
                let due_year: i32 = due.format("%Y").to_string().parse().unwrap_or(0);
                let due_month: u32 = due.format("%m").to_string().parse().unwrap_or(0);
                if due_year == year && due_month == month {
                    let key = due.format("%Y-%m-%d").to_string();
                    let bucket = days.entry(key).or_default();
                    bucket.0.push(json!({
                        "id": task.id,
                        "title": task.title,
                        "status": task.status,
                    }));
                }
            }
        }
        for milestone in state.milestones.iter().map(|entry| entry.value().clone()) {
            if milestone.project_id != project.id {
                continue;
            }
            if let Some(due) = milestone.due_date {
                let due_year: i32 = due.format("%Y").to_string().parse().unwrap_or(0);
                let due_month: u32 = due.format("%m").to_string().parse().unwrap_or(0);
                if due_year == year && due_month == month {
                    let key = due.format("%Y-%m-%d").to_string();
                    let bucket = days.entry(key).or_default();
                    bucket.1.push(json!({
                        "id": milestone.id,
                        "title": milestone.name,
                    }));
                }
            }
        }
        let day_rows: Vec<Value> = days
            .into_iter()
            .map(|(date, (tasks, milestones))| {
                json!({
                    "date": date,
                    "tasks": tasks,
                    "milestones": milestones,
                })
            })
            .collect();
        Ok(json!({
            "year": year,
            "month": month,
            "days": day_rows,
        }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Reports
// ---------------------------------------------------------------------------

async fn get_burndown_report(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Value> = (|| {
        let project = state.get_project(path.as_str())?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        require_capability(&state, viewer_role.as_deref(), "projects:reports")?;
        let sprint_id = params
            .filters
            .get("sprint_id")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                UnifiedError::bad_request("sprint_id query parameter is required")
            })?;
        let sprint = state
            .sprints
            .get(&sprint_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Sprint not found"))?;
        if sprint.project_id != project.id {
            return Err(UnifiedError::not_found("Sprint not found"));
        }
        let tasks: Vec<ProjectTask> = state
            .tasks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|task| {
                task.project_id == project.id
                    && task.sprint_id.as_deref() == Some(sprint_id.as_str())
            })
            .collect();
        let total_points =
            s5_round2(tasks.iter().map(|task| task.estimate_points.unwrap_or(0.0)).sum::<f64>());
        let today = now().date_naive();
        let start_day = sprint
            .start_date
            .map(|moment| moment.date_naive())
            .or_else(|| {
                tasks
                    .iter()
                    .map(|task| task.created_at.date_naive())
                    .min()
            })
            .unwrap_or(today);
        let mut end_day = sprint
            .end_date
            .map(|moment| moment.date_naive())
            .unwrap_or(today);
        if end_day < start_day {
            end_day = start_day;
        }
        // Cap the chart at one year so a bad date range cannot
        // produce an unbounded response.
        if end_day.signed_duration_since(start_day).num_days() > 365 {
            end_day = start_day + chrono::Duration::days(365);
        }
        let total_days = end_day.signed_duration_since(start_day).num_days();
        let mut day_rows: Vec<Value> = Vec::new();
        for offset in 0..=total_days {
            let day = start_day + chrono::Duration::days(offset);
            let day_end = s5_end_of_day(day);
            let ideal_remaining = if total_days == 0 {
                total_points
            } else {
                s5_round2(total_points * (1.0 - offset as f64 / total_days as f64)).max(0.0)
            };
            let actual_remaining = s5_round2(
                tasks
                    .iter()
                    .filter(|task| {
                        task.created_at <= day_end
                            && task
                                .completed_at
                                .is_none_or(|completed| completed > day_end)
                    })
                    .map(|task| task.estimate_points.unwrap_or(0.0))
                    .sum::<f64>(),
            );
            day_rows.push(json!({
                "date": day.format("%Y-%m-%d").to_string(),
                "ideal_remaining": ideal_remaining,
                "actual_remaining": actual_remaining,
            }));
        }
        Ok(json!({
            "sprint_id": sprint.id,
            "total_points": total_points,
            "days": day_rows,
        }))
    })();
    json::respond(result, false)
}

async fn get_velocity_report(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Value> = (|| {
        let project = state.get_project(path.as_str())?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        require_capability(&state, viewer_role.as_deref(), "projects:reports")?;
        let mut sprints: Vec<ProjectSprint> = state
            .sprints
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|sprint| {
                sprint.project_id == project.id
                    && matches!(sprint.status.as_str(), "completed" | "done" | "closed")
            })
            .collect();
        // Most recent first, then keep the last eight and present
        // them in chronological order for charting.
        sprints.sort_by(|a, b| {
            let end_a = a.end_date.unwrap_or(a.updated_at);
            let end_b = b.end_date.unwrap_or(b.updated_at);
            end_b.cmp(&end_a)
        });
        sprints.truncate(8);
        sprints.reverse();
        let rows: Vec<Value> = sprints
            .iter()
            .map(|sprint| {
                json!({
                    "sprint_id": sprint.id,
                    "name": sprint.name,
                    "planned": sprint.velocity_planned.unwrap_or(0.0),
                    "actual": sprint.velocity_actual.unwrap_or(0.0),
                })
            })
            .collect();
        let average_actual = if rows.is_empty() {
            0.0
        } else {
            s5_round2(
                sprints
                    .iter()
                    .map(|sprint| sprint.velocity_actual.unwrap_or(0.0))
                    .sum::<f64>()
                    / sprints.len() as f64,
            )
        };
        Ok(json!({
            "sprints": rows,
            "average_actual": average_actual,
        }))
    })();
    json::respond(result, false)
}

async fn get_cumulative_flow_report(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Value> = (|| {
        let project = state.get_project(path.as_str())?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        require_capability(&state, viewer_role.as_deref(), "projects:reports")?;
        let days = params
            .filters
            .get("days")
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(30)
            .clamp(1, 90);
        let tasks: Vec<ProjectTask> = state
            .tasks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|task| task.project_id == project.id)
            .collect();
        let today = now().date_naive();
        let mut day_rows: Vec<Value> = Vec::new();
        for offset in (0..days).rev() {
            let day = today - chrono::Duration::days(offset);
            let day_end = s5_end_of_day(day);
            // Approximation: a task is present on a day once created.
            // It counts as done from the day it completed; otherwise
            // it counts under its current status.
            let mut counts: std::collections::BTreeMap<String, u64> =
                std::collections::BTreeMap::new();
            for task in &tasks {
                if task.created_at > day_end {
                    continue;
                }
                let bucket = if task
                    .completed_at
                    .is_some_and(|completed| completed <= day_end)
                    || (s5_task_is_done(task) && task.updated_at <= day_end)
                {
                    "done".to_string()
                } else {
                    task.status.clone()
                };
                *counts.entry(bucket).or_insert(0) += 1;
            }
            let mut counts_json = serde_json::Map::new();
            for (status, count) in counts {
                counts_json.insert(status, json!(count));
            }
            day_rows.push(json!({
                "date": day.format("%Y-%m-%d").to_string(),
                "counts": counts_json,
            }));
        }
        Ok(json!({ "days": day_rows }))
    })();
    json::respond(result, false)
}

async fn get_cycle_time_report(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Value> = (|| {
        let project = state.get_project(path.as_str())?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        require_capability(&state, viewer_role.as_deref(), "projects:reports")?;
        let mut samples: Vec<(String, String, f64)> = state
            .tasks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|task| task.project_id == project.id)
            .filter_map(|task| {
                task.completed_at.map(|completed| {
                    let hours = s5_round2(
                        completed
                            .signed_duration_since(task.created_at)
                            .num_seconds() as f64
                            / 3600.0,
                    );
                    (task.id, task.title, hours)
                })
            })
            .collect();
        samples.sort_by(|a, b| {
            b.2.partial_cmp(&a.2)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut hours_sorted: Vec<f64> = samples.iter().map(|sample| sample.2).collect();
        hours_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let count = hours_sorted.len();
        let average_hours = if count == 0 {
            0.0
        } else {
            s5_round2(hours_sorted.iter().sum::<f64>() / count as f64)
        };
        let items: Vec<Value> = samples
            .iter()
            .map(|(task_id, title, hours)| {
                json!({
                    "task_id": task_id,
                    "title": title,
                    "hours": hours,
                })
            })
            .collect();
        Ok(json!({
            "average_hours": average_hours,
            "median_hours": s5_round2(s5_median(&hours_sorted)),
            "p90_hours": s5_round2(s5_percentile(&hours_sorted, 90.0)),
            "samples": count,
            "items": items,
        }))
    })();
    json::respond(result, false)
}

async fn get_critical_path_report(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Value> = (|| {
        let project = state.get_project(path.as_str())?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        require_capability(&state, viewer_role.as_deref(), "projects:reports")?;
        let tasks: Vec<ProjectTask> = state
            .tasks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|task| task.project_id == project.id)
            .collect();
        let task_ids: std::collections::HashSet<String> =
            tasks.iter().map(|task| task.id.clone()).collect();
        // One working day of lag counts as eight working hours.
        let lag_hours = |lag_days: i32| lag_days as f64 * 8.0;
        let durations: std::collections::HashMap<String, f64> = tasks
            .iter()
            .map(|task| {
                let hours = task.estimate_hours.unwrap_or(8.0);
                (task.id.clone(), if hours > 0.0 { hours } else { 8.0 })
            })
            .collect();
        // Edges run from a prerequisite to its dependent, using only
        // finish-to-start dependencies.
        let mut successors: std::collections::HashMap<String, Vec<(String, f64)>> =
            std::collections::HashMap::new();
        let mut predecessors: std::collections::HashMap<String, Vec<(String, f64)>> =
            std::collections::HashMap::new();
        for dependency in state.dependencies.iter().map(|entry| entry.value().clone()) {
            if dependency.kind != "finish_start" {
                continue;
            }
            if !task_ids.contains(&dependency.task_id)
                || !task_ids.contains(&dependency.depends_on_task_id)
            {
                continue;
            }
            let lag = lag_hours(dependency.lag_days);
            successors
                .entry(dependency.depends_on_task_id.clone())
                .or_default()
                .push((dependency.task_id.clone(), lag));
            predecessors
                .entry(dependency.task_id.clone())
                .or_default()
                .push((dependency.depends_on_task_id.clone(), lag));
        }
        // Kahn topological sort.
        let mut indegree: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for task in &tasks {
            indegree.insert(task.id.clone(), 0);
        }
        for (_, edges) in &successors {
            for (to, _) in edges {
                if let Some(count) = indegree.get_mut(to) {
                    *count += 1;
                }
            }
        }
        let mut queue: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        for task in &tasks {
            if indegree.get(&task.id).copied().unwrap_or(0) == 0 {
                queue.push_back(task.id.clone());
            }
        }
        let mut order: Vec<String> = Vec::new();
        while let Some(node) = queue.pop_front() {
            order.push(node.clone());
            if let Some(edges) = successors.get(&node) {
                for (to, _) in edges {
                    if let Some(count) = indegree.get_mut(to) {
                        *count -= 1;
                        if *count == 0 {
                            queue.push_back(to.clone());
                        }
                    }
                }
            }
        }
        if order.len() != tasks.len() {
            // Not a DAG: recover one concrete cycle to name it.
            let plain_successors: std::collections::HashMap<String, Vec<String>> = successors
                .iter()
                .map(|(from, edges)| {
                    (
                        from.clone(),
                        edges.iter().map(|(to, _)| to.clone()).collect(),
                    )
                })
                .collect();
            let mut visit_state: std::collections::HashMap<String, u8> =
                std::collections::HashMap::new();
            let mut cycle: Vec<String> = Vec::new();
            for task in &tasks {
                if visit_state.get(&task.id).copied().unwrap_or(0) == 0 {
                    let mut stack: Vec<String> = Vec::new();
                    if let Some(found) =
                        s5_cycle_dfs(&task.id, &plain_successors, &mut visit_state, &mut stack)
                    {
                        cycle = found;
                        break;
                    }
                }
            }
            let description = if cycle.is_empty() {
                "dependency cycle detected".to_string()
            } else {
                format!("Dependency cycle detected: {}", cycle.join(" -> "))
            };
            return Err(UnifiedError::bad_request(description));
        }
        // Forward pass: earliest start/finish.
        let mut earliest_start: std::collections::HashMap<String, f64> =
            std::collections::HashMap::new();
        let mut earliest_finish: std::collections::HashMap<String, f64> =
            std::collections::HashMap::new();
        for node in &order {
            let start = predecessors
                .get(node)
                .map(|edges| {
                    edges
                        .iter()
                        .map(|(from, lag)| {
                            earliest_finish.get(from).copied().unwrap_or(0.0) + lag
                        })
                        .fold(0.0, f64::max)
                })
                .unwrap_or(0.0);
            let duration = durations.get(node).copied().unwrap_or(8.0);
            earliest_start.insert(node.clone(), start);
            earliest_finish.insert(node.clone(), start + duration);
        }
        let total_duration_hours = s5_round2(
            earliest_finish
                .values()
                .copied()
                .fold(0.0, f64::max),
        );
        // Backward pass: latest finish/start.
        let mut latest_finish: std::collections::HashMap<String, f64> =
            std::collections::HashMap::new();
        let mut latest_start: std::collections::HashMap<String, f64> =
            std::collections::HashMap::new();
        for node in order.iter().rev() {
            let finish = successors
                .get(node)
                .map(|edges| {
                    edges
                        .iter()
                        .map(|(to, lag)| {
                            latest_start.get(to).copied().unwrap_or(total_duration_hours) - lag
                        })
                        .fold(f64::INFINITY, f64::min)
                })
                .filter(|value| value.is_finite())
                .unwrap_or(total_duration_hours);
            let duration = durations.get(node).copied().unwrap_or(8.0);
            latest_finish.insert(node.clone(), finish);
            latest_start.insert(node.clone(), finish - duration);
        }
        let by_id: std::collections::HashMap<String, &ProjectTask> =
            tasks.iter().map(|task| (task.id.clone(), task)).collect();
        let mut rows: Vec<Value> = Vec::new();
        let mut critical_task_ids: Vec<String> = Vec::new();
        for node in &order {
            let early_start = earliest_start.get(node).copied().unwrap_or(0.0);
            let early_finish = earliest_finish.get(node).copied().unwrap_or(0.0);
            let late_start = latest_start.get(node).copied().unwrap_or(0.0);
            let late_finish = latest_finish.get(node).copied().unwrap_or(0.0);
            let float_hours = s5_round2(late_start - early_start);
            let critical = float_hours.abs() < 1e-9;
            if critical {
                critical_task_ids.push(node.clone());
            }
            let title = by_id
                .get(node)
                .map(|task| task.title.clone())
                .unwrap_or_default();
            rows.push(json!({
                "task_id": node,
                "title": title,
                "earliest_start_hours": s5_round2(early_start),
                "earliest_finish_hours": s5_round2(early_finish),
                "latest_start_hours": s5_round2(late_start),
                "latest_finish_hours": s5_round2(late_finish),
                "float_hours": float_hours,
                "critical": critical,
            }));
        }
        Ok(json!({
            "critical_task_ids": critical_task_ids,
            "total_duration_hours": total_duration_hours,
            "tasks": rows,
        }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Import / export
// ---------------------------------------------------------------------------

async fn export_project(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Value> = (|| {
        let project = state.get_project(path.as_str())?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        require_capability(&state, viewer_role.as_deref(), "projects:manage")?;
        let format = params
            .filters
            .get("format")
            .map(|value| value.trim().to_lowercase())
            .unwrap_or_else(|| "json".to_string());
        if format != "json" && format != "csv" {
            return Err(UnifiedError::bad_request(
                "format must be csv or json",
            ));
        }
        let mut tasks: Vec<ProjectTask> = state
            .tasks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|task| task.project_id == project.id)
            .collect();
        tasks.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if format == "csv" {
            let mut content = String::from(
                "id,title,description,task_type,status,priority,assignee_id,estimate_points,estimate_hours,due_date,labels\n",
            );
            for task in &tasks {
                let row = [
                    s5_csv_escape(&task.id),
                    s5_csv_escape(&task.title),
                    s5_csv_escape(&task.description),
                    s5_csv_escape(&task.task_type),
                    s5_csv_escape(&task.status),
                    s5_csv_escape(&task.priority),
                    s5_csv_escape(task.assignee_id.as_deref().unwrap_or("")),
                    s5_csv_number(task.estimate_points),
                    s5_csv_number(task.estimate_hours),
                    s5_csv_escape(&s5_csv_datetime(task.due_date)),
                    s5_csv_escape(&task.labels.join(";")),
                ];
                content.push_str(&row.join(","));
                content.push('\n');
            }
            return Ok(json!({
                "format": "csv",
                "filename": format!("project-{}-tasks.csv", project.id),
                "content": content,
            }));
        }
        let mut milestones: Vec<_> = state
            .milestones
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|milestone| milestone.project_id == project.id)
            .collect();
        milestones.sort_by(|a, b| a.name.cmp(&b.name));
        let mut sprints: Vec<ProjectSprint> = state
            .sprints
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|sprint| sprint.project_id == project.id)
            .collect();
        sprints.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let mut phases: Vec<_> = state
            .phases
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|phase| phase.project_id == project.id)
            .collect();
        phases.sort_by(|a, b| a.name.cmp(&b.name));
        let mut time_entries: Vec<ProjectTimeEntry> = state
            .time_entries
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.project_id == project.id)
            .collect();
        time_entries.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(json!({
            "project": project,
            "tasks": tasks,
            "milestones": milestones,
            "sprints": sprints,
            "phases": phases,
            "time_entries": time_entries,
        }))
    })();
    json::respond(result, false)
}

async fn import_project(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<ImportProject>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result: UnifiedResult<Value> = (|| {
        let project = state.get_project(path.as_str())?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        require_capability(&state, viewer_role.as_deref(), "projects:manage")?;
        let input = body.into_inner();
        if input.format.trim().to_lowercase() != "json" {
            return Err(UnifiedError::bad_request(
                "format must be json",
            ));
        }
        let reporter_id = viewer_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| project.owner_id.clone());
        let mut imported: u64 = 0;
        let mut skipped: u64 = 0;
        let mut errors: Vec<Value> = Vec::new();
        for (index, row) in input.tasks.into_iter().enumerate() {
            let title = row.title.map(|value| value.trim().to_string());
            let title = match title {
                Some(value) if !value.is_empty() => value,
                _ => {
                    skipped += 1;
                    errors.push(json!({
                        "row": index,
                        "error": "title is required",
                    }));
                    continue;
                }
            };
            if let Some(points) = row.estimate_points {
                if points < 0.0 {
                    skipped += 1;
                    errors.push(json!({
                        "row": index,
                        "title": title,
                        "error": "estimate_points must not be negative",
                    }));
                    continue;
                }
            }
            if let Some(hours) = row.estimate_hours {
                if hours < 0.0 {
                    skipped += 1;
                    errors.push(json!({
                        "row": index,
                        "title": title,
                        "error": "estimate_hours must not be negative",
                    }));
                    continue;
                }
            }
            let task_type = row
                .task_type
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "task".to_string());
            let status = row
                .status
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "todo".to_string());
            let priority = row
                .priority
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "normal".to_string());
            let task = ProjectTask {
                id: new_id(),
                project_id: project.id.clone(),
                board_id: None,
                parent_id: None,
                epic_id: None,
                title,
                description: row
                    .description
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_default(),
                task_type,
                status,
                priority,
                assignee_id: row
                    .assignee_id
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty()),
                reporter_id: reporter_id.clone(),
                estimate_points: row.estimate_points,
                estimate_hours: row.estimate_hours,
                labels: Vec::new(),
                custom_values: Value::Null,
                start_date: None,
                due_date: row.due_date,
                completed_at: None,
                sprint_id: None,
                phase_id: None,
                cycle_id: None,
                pi_id: None,
                milestone_id: None,
                process_group: None,
                knowledge_area: None,
                waste_flag: None,
                pair_partner_id: None,
                test_first: false,
                dmaic_phase: None,
                appetite_weeks: None,
                created_at: now(),
                updated_at: now(),
            };
            state.tasks.insert(task.id.clone(), task);
            imported += 1;
        }
        state.log_activity(
            &project.id,
            &reporter_id,
            "project.imported",
            "project",
            Some(project.id.clone()),
            json!({
                "imported": imported,
                "skipped": skipped,
            }),
        );
        Ok(json!({
            "imported": imported,
            "skipped": skipped,
            "errors": errors,
        }))
    })();
    s5_respond_created(result)
}

// ---------------------------------------------------------------------------
// Activity
// ---------------------------------------------------------------------------

async fn list_project_activity(
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&params);
    let result = (|| {
        let project = state.get_project(path.as_str())?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
        )?;
        require_capability(&state, viewer_role.as_deref(), "projects:view")?;
        let mut items: Vec<ProjectActivity> = state
            .activity
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|activity| {
                activity.project_id == project.id
                    && params
                        .filters
                        .get("actor_id")
                        .is_none_or(|value| activity.actor_id == *value)
                    && params
                        .filters
                        .get("action")
                        .is_none_or(|value| activity.action == *value)
                    && params.filters.get("target_type").is_none_or(|value| {
                        activity.target_type == *value
                    })
                    && matches_search(
                        &format!(
                            "{} {} {}",
                            activity.action, activity.target_type, activity.actor_id
                        ),
                        &params,
                    )
            })
            .collect();
        // Newest first by default; an explicit asc order is honoured.
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.order.as_deref() != Some("asc") {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}
