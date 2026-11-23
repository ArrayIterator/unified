// Diesel models for the departments domain of the unified-db-infra bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Departments (CORE) (2026-11-22-000026_departments).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
// NOTE: the path must stay fully qualified to the TABLE module.
// `crate::schema::departments` is the schema file-module
// (schema/departments.rs), not the table: the
// `pub use departments::*;` re-export in schema.rs cannot surface
// the inner `departments` table over the file-module of the same
// name, so the table's columns live at
// `crate::schema::departments::departments`. Pointing `table_name`
// at the file-module (or the bare name `departments`) makes every
// field fail with E0425 "cannot find type ... in module
// `departments`".
#[diesel(table_name = crate::schema::departments::departments)]
pub struct Department {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub email: Option<String>,
    pub status: String,
    pub is_default: bool,
    pub sla_first_response_minutes: i32,
    pub sla_resolution_minutes: i32,
    pub working_hours: Value,
    pub auto_reply: Value,
    pub agents: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = crate::schema::departments::departments)]
pub struct NewDepartment {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = department_routing_rules)]
pub struct DepartmentRoutingRule {
    pub id: Uuid,
    pub department_id: Uuid,
    pub match_type: String,
    pub match_value: String,
    pub sort_order: i32,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = department_routing_rules)]
pub struct NewDepartmentRoutingRule {
    pub id: Uuid,
    pub department_id: Uuid,
    pub match_type: String,
    pub match_value: String,
}
