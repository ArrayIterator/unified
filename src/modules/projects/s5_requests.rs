// Projects module — time entry and import inputs.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use chrono::{DateTime, Utc};
use serde::Deserialize;

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
