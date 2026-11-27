// Projects module - sprint, phase, cycle, program increment, and business case inputs.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

// Section s4 of the Projects module: methodology execution containers.
//
// Sprints (Scrum / Scrumban / Agile), phases (Waterfall / PRINCE2 stages /
// DMAIC / Hybrid), Shape Up cycles, SAFe program increments, and PRINCE2
// business cases. Imports, entity structs, the state type, the shared
// helpers (`now`, `new_id`, `paginate`, `viewer_of`, `matches_search`,
// `require_capability`, `require_project_access`, `methodology_profile`)
// and the route table live in section s1; this file only adds the input
// structs and handlers named there.

// Input structs

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
