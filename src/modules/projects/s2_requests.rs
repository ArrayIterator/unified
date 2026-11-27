// Projects module - project, member, template, board, and milestone inputs.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

// Section s2 of the Projects module: projects, members, methodologies,
// templates, boards, columns, and milestones.
//
// This file is assembled into `src/modules/projects.rs` after s1. It
// defines no imports, no module header, no entity structs, and no
// `Module` implementation; those live in s1. Every helper defined here
// is prefixed with `s2_` so it cannot collide with other sections.
//
// Viewer identity for every handler comes from the request
// headers (`X-User-Id` / `X-Role`) through the s1 helper
// `viewer_of`, so handlers take an `HttpRequest` extractor.

// Input structs

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
