// Projects module - methodology profiles, submodules, and product flags.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use serde::Serialize;

// Methodologies

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

// Submodule declarations and product flag
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
    PROJECT_SUBMODULES
        .iter()
        .find(|submodule| submodule.id == id)
}

/// True when this module can be sold as a product.
pub const AVAILABLE_AS_PRODUCT: bool = true;

/// Product category this module belongs to when sold as a product.
pub const PRODUCT_CATEGORY: &str = "productivity";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methodology_profile_finds_all_fifteen() {
        for id in [
            "kanban",
            "scrum",
            "waterfall",
            "scrumban",
            "agile",
            "lean",
            "xp",
            "prince2",
            "pmbok",
            "cpm",
            "ccpm",
            "six_sigma",
            "hybrid",
            "shape_up",
            "safe",
        ] {
            assert!(methodology_profile(id).is_some(), "{id} should exist");
        }
        assert_eq!(METHODOLOGIES.len(), 15);
        assert!(methodology_profile("unknown").is_none());
    }

    #[test]
    fn valid_methodology_matches_the_registry() {
        assert!(valid_methodology("scrum"));
        assert!(valid_methodology("safe"));
        assert!(!valid_methodology("scrum2"));
        assert!(!valid_methodology(""));
    }

    #[test]
    fn scrum_profile_uses_sprints_and_story_points() {
        let profile = methodology_profile("scrum").unwrap();
        assert!(profile.uses_sprints);
        assert!(profile.uses_story_points);
        assert!(!profile.uses_phases);
        assert_eq!(profile.default_statuses[0], "backlog");
    }

    #[test]
    fn methodology_submodule_lookup() {
        assert!(methodology_submodule("scrum").is_some());
        assert!(methodology_submodule("nope").is_none());
        assert!(AVAILABLE_AS_PRODUCT);
        assert_eq!(PRODUCT_CATEGORY, "productivity");
    }
}
